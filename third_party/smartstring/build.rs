// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

fn main() {
    // `Allocator` exists on stable Rust 1.92, but its methods remain unstable.
    // The upstream trait-existence probe therefore emits `has_allocator` on
    // Linux and makes stable builds compile the unstable zero-copy conversion
    // path. Keep both cfgs disabled so the crate uses its safe copying fallback.
    println!("cargo:rustc-check-cfg=cfg(has_allocator)");
    println!("cargo:rustc-check-cfg=cfg(needs_allocator_feature)");
    println!("cargo:rerun-if-changed=build.rs");
}
