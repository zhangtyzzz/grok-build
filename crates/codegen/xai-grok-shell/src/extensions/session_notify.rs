//! Reliable, live-session notification ingress for out-of-process agents.
//!
//! `x.ai/session/notify` is intentionally scoped to a session already resident
//! in this agent (normally the shared leader). It never loads or resumes a
//! session from disk, which avoids two processes concurrently owning the same
//! session. The actor acknowledges after serialized queue acceptance.
//!
//! Notification-id deduplication is bounded to the resident actor instance.
//! A future persistence-backed inbox can strengthen this across actor reloads
//! and leader restarts without changing the wire contract.

use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::{ExternalNotifyAck, SessionCommand};

pub const MAX_NOTIFICATION_TEXT_BYTES: usize = 256 * 1024;
const MAX_SESSION_ID_BYTES: usize = 512;
const MAX_NOTIFICATION_ID_BYTES: usize = 256;
const MAX_NOTIFICATION_KIND_BYTES: usize = 64;
const MAX_DEDUPE_ENTRIES: usize = 4096;
const ACTOR_ACK_TIMEOUT: Duration = Duration::from_secs(10);

fn is_disallowed_text_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\t')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotifyRequest {
    pub session_id: String,
    pub notification_id: String,
    pub kind: String,
    pub text: String,
    /// Start a model turn if the target session is idle.
    #[serde(default)]
    pub wake: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionNotifyStatus {
    Queued,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotifyResponse {
    pub status: SessionNotifyStatus,
    pub notification_id: String,
    pub turn_running: bool,
    pub will_wake: bool,
    /// Documents the current retry guarantee without baking implementation
    /// detail into the status value.
    pub dedupe_scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReserveOutcome {
    Reserved,
    Pending,
    AcceptedDuplicate,
    AtCapacity,
}

#[derive(Clone, Debug)]
struct NotificationKey {
    session_id: String,
    notification_id: String,
    /// Retaining the actor-owned Arc makes its pointer a stable generation
    /// identity and prevents allocator address reuse while this key exists.
    actor_instance: Arc<Mutex<Option<String>>>,
}

impl NotificationKey {
    fn new(
        session_id: String,
        notification_id: String,
        actor_instance: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            session_id,
            notification_id,
            actor_instance,
        }
    }
}

impl PartialEq for NotificationKey {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.notification_id == other.notification_id
            && Arc::ptr_eq(&self.actor_instance, &other.actor_instance)
    }
}

impl Eq for NotificationKey {}

impl Hash for NotificationKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.session_id.hash(state);
        self.notification_id.hash(state);
        Arc::as_ptr(&self.actor_instance).hash(state);
    }
}

#[derive(Default)]
struct NotificationDedupe {
    /// Commands reserved while their actor ACK is still outstanding. These
    /// entries are never evicted: the mailbox may already contain the command.
    pending: HashSet<NotificationKey>,
    /// ACKed or unknown-outcome deliveries. These remain deduplicated, but are
    /// the only entries eligible for bounded FIFO eviction.
    accepted: HashSet<NotificationKey>,
    order: VecDeque<NotificationKey>,
}

impl NotificationDedupe {
    fn reserve(&mut self, key: NotificationKey) -> ReserveOutcome {
        if self.pending.contains(&key) {
            return ReserveOutcome::Pending;
        }
        if self.accepted.contains(&key) {
            return ReserveOutcome::AcceptedDuplicate;
        }

        while self.pending.len() + self.accepted.len() >= MAX_DEDUPE_ENTRIES {
            if let Some(evicted) = self.order.pop_front() {
                self.accepted.remove(&evicted);
            } else {
                // Every slot is awaiting an actor ACK. Evicting one could let
                // a concurrent retry inject the same notification twice.
                return ReserveOutcome::AtCapacity;
            }
        }
        self.pending.insert(key);
        ReserveOutcome::Reserved
    }

    /// Mark a mailbox-accepted reservation as durable for this resident actor
    /// generation. Both a positive ACK and an unknown ACK outcome land here:
    /// after mailbox send succeeds, neither is safe to retry on this actor.
    fn accept(&mut self, key: &NotificationKey) {
        if self.pending.remove(key) {
            self.accepted.insert(key.clone());
            self.order.push_back(key.clone());
        }
    }

    /// Release a reservation only when the command could not enter the actor
    /// mailbox. Once `send` succeeds, an ACK failure has an unknown delivery
    /// outcome, so retaining the reservation is the only way to prevent a
    /// retry from injecting the same notification twice.
    fn release(&mut self, key: &NotificationKey) {
        self.pending.remove(key);
    }
}

fn notification_dedupe() -> &'static Mutex<NotificationDedupe> {
    static DEDUPE: OnceLock<Mutex<NotificationDedupe>> = OnceLock::new();
    DEDUPE.get_or_init(|| Mutex::new(NotificationDedupe::default()))
}

fn with_dedupe<T>(f: impl FnOnce(&mut NotificationDedupe) -> T) -> T {
    let mut guard = notification_dedupe()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Once a command has entered the actor mailbox, cancellation of the RPC
/// future must not strand its key in `pending` forever. Dropping this guard
/// records the delivery as accepted/unknown, which is the conservative retry
/// contract whether the ACK arrived, timed out, or the client disconnected.
struct SentReservationGuard {
    key: NotificationKey,
}

impl SentReservationGuard {
    fn new(key: NotificationKey) -> Self {
        Self { key }
    }
}

impl Drop for SentReservationGuard {
    fn drop(&mut self) {
        with_dedupe(|dedupe| dedupe.accept(&self.key));
    }
}

fn validate_request(req: &SessionNotifyRequest) -> Result<(), acp::Error> {
    if req.session_id.trim().is_empty()
        || req.session_id.len() > MAX_SESSION_ID_BYTES
        || req.session_id.chars().any(char::is_control)
    {
        return Err(acp::Error::invalid_params().data(format!(
            "sessionId must be 1-{MAX_SESSION_ID_BYTES} bytes and contain no control characters"
        )));
    }
    if req.notification_id.is_empty()
        || req.notification_id.len() > MAX_NOTIFICATION_ID_BYTES
        || req.notification_id.chars().any(char::is_control)
    {
        return Err(acp::Error::invalid_params().data(format!(
            "notificationId must be 1-{MAX_NOTIFICATION_ID_BYTES} bytes and contain no control characters"
        )));
    }
    if req.kind.is_empty()
        || req.kind.len() > MAX_NOTIFICATION_KIND_BYTES
        || !req
            .kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(acp::Error::invalid_params().data(format!(
            "kind must be 1-{MAX_NOTIFICATION_KIND_BYTES} ASCII letters, digits, '.', '-' or '_'"
        )));
    }
    if req.text.trim().is_empty() {
        return Err(acp::Error::invalid_params().data("text must not be empty"));
    }
    if req.text.len() > MAX_NOTIFICATION_TEXT_BYTES {
        return Err(acp::Error::invalid_params().data(format!(
            "text exceeds the {MAX_NOTIFICATION_TEXT_BYTES}-byte limit"
        )));
    }
    if req.text.chars().any(is_disallowed_text_control) {
        return Err(acp::Error::invalid_params()
            .data("text may contain newlines and tabs, but no other control characters"));
    }
    Ok(())
}

fn response(
    status: SessionNotifyStatus,
    notification_id: String,
    ack: Option<ExternalNotifyAck>,
) -> SessionNotifyResponse {
    let ack = ack.unwrap_or(ExternalNotifyAck {
        turn_running: false,
        will_wake: false,
    });
    SessionNotifyResponse {
        status,
        notification_id,
        turn_running: ack.turn_running,
        will_wake: ack.will_wake,
        dedupe_scope: "resident_actor".to_string(),
    }
}

#[tracing::instrument(
    skip_all,
    fields(
        session_id = %req.session_id,
        notification_id = %req.notification_id,
        kind = %req.kind,
        wake = req.wake,
    )
)]
async fn notify(agent: &MvpAgent, req: SessionNotifyRequest) -> ExtResult {
    validate_request(&req)?;

    let sid = acp::SessionId::new(req.session_id.as_str());
    let Some(session) = agent.session_handle_waiting_for_load(&sid).await else {
        return Err(acp::Error::invalid_params().data(format!(
            "session is not live in the running leader: {}",
            req.session_id
        )));
    };

    let dedupe_key = NotificationKey::new(
        req.session_id.clone(),
        req.notification_id.clone(),
        session.current_prompt_id.clone(),
    );
    let (respond_to, response_rx) = oneshot::channel();
    enum MailboxOutcome {
        Sent,
        Pending,
        AcceptedDuplicate,
        AtCapacity,
        ActorClosed,
    }
    // Reserving and the non-awaiting mailbox send share one lock scope. A
    // concurrent duplicate can therefore never observe a reservation whose
    // command subsequently fails to enter the mailbox.
    let mailbox_outcome = with_dedupe(|dedupe| match dedupe.reserve(dedupe_key.clone()) {
        ReserveOutcome::Pending => MailboxOutcome::Pending,
        ReserveOutcome::AcceptedDuplicate => MailboxOutcome::AcceptedDuplicate,
        ReserveOutcome::AtCapacity => MailboxOutcome::AtCapacity,
        ReserveOutcome::Reserved => {
            if session
                .cmd_tx
                .send(SessionCommand::ExternalNotify {
                    notification_id: req.notification_id.clone(),
                    kind: req.kind,
                    text: req.text,
                    wake: req.wake,
                    respond_to,
                })
                .is_err()
            {
                dedupe.release(&dedupe_key);
                MailboxOutcome::ActorClosed
            } else {
                MailboxOutcome::Sent
            }
        }
    });
    match mailbox_outcome {
        MailboxOutcome::Sent => {}
        MailboxOutcome::Pending => {
            return Err(acp::Error::internal_error().data(
                "notification is still awaiting actor acknowledgement; delivery is pending, not yet confirmed duplicate",
            ));
        }
        MailboxOutcome::AcceptedDuplicate => {
            return to_raw_response(&response(
                SessionNotifyStatus::Duplicate,
                req.notification_id,
                None,
            ));
        }
        MailboxOutcome::AtCapacity => {
            return Err(acp::Error::internal_error().data(
                "notification dedupe capacity is occupied by deliveries awaiting actor acknowledgement; retry later",
            ));
        }
        MailboxOutcome::ActorClosed => {
            return Err(
                acp::Error::internal_error().data("target session actor is no longer running")
            );
        }
    }

    let _sent_reservation = SentReservationGuard::new(dedupe_key.clone());
    let ack = match tokio::time::timeout(ACTOR_ACK_TIMEOUT, response_rx).await {
        Ok(Ok(ack)) => ack,
        Ok(Err(_)) => {
            return Err(acp::Error::internal_error().data(
                "target session actor closed before acknowledging; delivery outcome is unknown and retries with the same notificationId are deduplicated",
            ));
        }
        Err(_) => {
            return Err(acp::Error::internal_error().data(format!(
                "target session actor did not acknowledge within {} seconds; delivery outcome is unknown and retries with the same notificationId are deduplicated",
                ACTOR_ACK_TIMEOUT.as_secs()
            )));
        }
    };

    to_raw_response(&response(
        SessionNotifyStatus::Queued,
        req.notification_id,
        Some(ack),
    ))
}

/// Handle `x.ai/session/notify`.
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/session/notify" => notify(agent, parse_params(args)?).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SessionNotifyRequest {
        SessionNotifyRequest {
            session_id: "session-1".to_string(),
            notification_id: "review:repo:abc123".to_string(),
            kind: "reviewer".to_string(),
            text: "No blocking findings.".to_string(),
            wake: true,
        }
    }

    fn actor_instance() -> Arc<Mutex<Option<String>>> {
        Arc::new(Mutex::new(None))
    }

    fn key(
        actor: &Arc<Mutex<Option<String>>>,
        session_id: &str,
        notification_id: impl Into<String>,
    ) -> NotificationKey {
        NotificationKey::new(
            session_id.to_string(),
            notification_id.into(),
            actor.clone(),
        )
    }

    #[test]
    fn validates_expected_reviewer_request() {
        validate_request(&request()).expect("valid reviewer notification");
    }

    #[test]
    fn rejects_empty_and_oversized_text() {
        let mut req = request();
        req.text = "  ".to_string();
        assert!(validate_request(&req).is_err());

        req.text = "x".repeat(MAX_NOTIFICATION_TEXT_BYTES + 1);
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn rejects_terminal_controls_in_text_but_allows_multiline_reviews() {
        let mut req = request();
        for unsafe_text in [
            "escape\u{001b}[31mred",
            "nul\u{0000}byte",
            "c1\u{0085}next-line",
            "carriage\rreturn",
        ] {
            req.text = unsafe_text.to_string();
            assert!(
                validate_request(&req).is_err(),
                "unsafe control must be rejected: {unsafe_text:?}"
            );
        }

        req.text = "Finding one\n\tIndented evidence".to_string();
        validate_request(&req).expect("newlines and tabs are safe review formatting");
    }

    #[test]
    fn rejects_unsafe_or_oversized_session_id() {
        let mut req = request();
        req.session_id = "session\nforged-log".to_string();
        assert!(validate_request(&req).is_err());

        req.session_id = "s".repeat(MAX_SESSION_ID_BYTES + 1);
        assert!(validate_request(&req).is_err());

        req.session_id = "session-safe".to_string();
        validate_request(&req).expect("bounded session id without controls");
    }

    #[test]
    fn rejects_unsafe_kind_but_allows_structured_id() {
        let mut req = request();
        req.kind = "reviewer agent".to_string();
        assert!(validate_request(&req).is_err());

        req.kind = "commit-review.v1".to_string();
        validate_request(&req).expect("safe kind and colon-separated id");
    }

    #[test]
    fn dedupe_is_session_scoped_and_pre_mailbox_failure_can_release() {
        let mut dedupe = NotificationDedupe::default();
        let actor = actor_instance();
        let a = key(&actor, "session-a", "review:abc");
        let b = key(&actor, "session-b", "review:abc");

        assert_eq!(dedupe.reserve(a.clone()), ReserveOutcome::Reserved);
        assert_eq!(dedupe.reserve(a.clone()), ReserveOutcome::Pending);
        assert_eq!(dedupe.reserve(b), ReserveOutcome::Reserved);

        dedupe.release(&a);
        assert_eq!(dedupe.reserve(a), ReserveOutcome::Reserved);
    }

    #[test]
    fn accepted_mailbox_reservation_stays_reserved_without_an_ack() {
        let mut dedupe = NotificationDedupe::default();
        let actor = actor_instance();
        let key = key(&actor, "session-a", "review:unknown");

        assert_eq!(dedupe.reserve(key.clone()), ReserveOutcome::Reserved);
        dedupe.accept(&key);
        // An ACK timeout or dropped oneshot transitions to accepted/unknown,
        // never to released: the actor may already have applied it.
        assert_eq!(dedupe.reserve(key), ReserveOutcome::AcceptedDuplicate);
    }

    #[test]
    fn dropping_sent_reservation_guard_cannot_strand_pending_key() {
        let actor = actor_instance();
        let key = key(
            &actor,
            "session-guard-test",
            format!("review:guard:{}", uuid::Uuid::now_v7()),
        );
        assert_eq!(
            with_dedupe(|dedupe| dedupe.reserve(key.clone())),
            ReserveOutcome::Reserved
        );
        drop(SentReservationGuard::new(key.clone()));
        assert_eq!(
            with_dedupe(|dedupe| dedupe.reserve(key)),
            ReserveOutcome::AcceptedDuplicate
        );
    }

    #[test]
    fn dedupe_is_bound_to_the_resident_actor_generation() {
        let mut dedupe = NotificationDedupe::default();
        let old_actor = actor_instance();
        let new_actor = actor_instance();
        let old_key = key(&old_actor, "session-a", "review:same");
        let new_key = key(&new_actor, "session-a", "review:same");

        assert_eq!(dedupe.reserve(old_key.clone()), ReserveOutcome::Reserved);
        dedupe.accept(&old_key);
        assert_eq!(dedupe.reserve(old_key), ReserveOutcome::AcceptedDuplicate);
        assert_eq!(
            dedupe.reserve(new_key),
            ReserveOutcome::Reserved,
            "a reloaded actor may retry a notification whose transient payload was lost with the old actor"
        );
    }

    #[test]
    fn capacity_never_evicts_pending_actor_ack_reservations() {
        let mut dedupe = NotificationDedupe::default();
        let actor = actor_instance();
        let keys: Vec<_> = (0..MAX_DEDUPE_ENTRIES)
            .map(|i| key(&actor, "session-a", format!("review:pending:{i}")))
            .collect();
        for key in &keys {
            assert_eq!(
                dedupe.reserve(key.clone()),
                ReserveOutcome::Reserved,
                "fill pending reservation"
            );
        }

        let overflow = key(&actor, "session-a", "review:overflow");
        assert_eq!(
            dedupe.reserve(overflow.clone()),
            ReserveOutcome::AtCapacity,
            "a full pending set must reject instead of evicting an in-flight key"
        );
        assert_eq!(
            dedupe.reserve(keys[1].clone()),
            ReserveOutcome::Pending,
            "pending keys remain reserved under capacity pressure"
        );

        // Once one delivery leaves the ACK window it becomes the sole
        // evictable entry, so the overflow request can make bounded progress.
        dedupe.accept(&keys[0]);
        assert_eq!(
            dedupe.reserve(overflow),
            ReserveOutcome::Reserved,
            "accepted entries, not pending entries, provide eviction capacity"
        );
        assert_eq!(
            dedupe.reserve(keys[1].clone()),
            ReserveOutcome::Pending,
            "other pending keys were not evicted"
        );
    }

    #[test]
    fn response_exposes_resident_actor_durability() {
        let result = response(
            SessionNotifyStatus::Queued,
            "review:abc".to_string(),
            Some(ExternalNotifyAck {
                turn_running: false,
                will_wake: true,
            }),
        );
        assert_eq!(result.status, SessionNotifyStatus::Queued);
        assert!(result.will_wake);
        assert_eq!(result.dedupe_scope, "resident_actor");
    }
}
