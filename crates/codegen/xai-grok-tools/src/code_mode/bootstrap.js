(() => {
  const parse = JSON.parse.bind(JSON);
  const stringify = JSON.stringify.bind(JSON);
  const keys = Object.keys.bind(Object);
  const freeze = Object.freeze.bind(Object);
  const isSafeInteger = Number.isSafeInteger.bind(Number);
  const isInteger = Number.isInteger.bind(Number);
  const isFinite = Number.isFinite.bind(Number);
  const PromiseClass = Promise;
  const ErrorClass = Error;
  const call = globalThis.__hostCall;
  const log = globalThis.__hostLog;
  const inventory = parse(globalThis.__inventoryJson);
  delete globalThis.__hostCall;
  delete globalThis.__hostLog;
  delete globalThis.__inventoryJson;
  // Null-prototype record: user code cannot redirect replies by patching Map.prototype.
  const pending = Object.create(null);
  const serialize = value => stringify(value === undefined ? null : value, (_key, v) => {
    if (typeof v === 'undefined' || typeof v === 'function' || typeof v === 'symbol' || typeof v === 'bigint' || v instanceof PromiseClass)
      throw new ErrorClass('Unsupported Code Mode data; await promises and return JSON data');
    if (typeof v === 'number' && (!isFinite(v) || (isInteger(v) && !isSafeInteger(v))))
      throw new ErrorClass('Unsupported non-finite or imprecise Code Mode number');
    return v;
  });
  const tools = Object.create(null);
  for (const entry of inventory) {
    const name = entry.name;
    tools[name] = input => new PromiseClass((resolve, reject) => {
      const id = call(name, serialize(input));
      pending[id] = {resolve, reject};
    });
  }
  globalThis.tools = freeze(tools);
  globalThis.catalog = freeze({
    search(query = '', offset = 0) {
      if (!isSafeInteger(offset) || offset < 0) throw new ErrorClass('Invalid catalog offset');
      const needle = String(query).toLowerCase();
      const matches = inventory.filter(t =>
        t.name.toLowerCase().includes(needle) || String(t.description || '').toLowerCase().includes(needle));
      return {total: matches.length, tools: matches.slice(offset, offset + 20).map(t => ({name: t.name, description: t.description}))};
    },
    describe(name) {
      const entry = inventory.find(t => t.name === name);
      if (!entry) throw new ErrorClass('Unknown tool; use catalog.search()');
      return parse(stringify(entry));
    },
  });
  globalThis.console = freeze({log: (...values) => log(values.map(v => serialize(v)).join(' '))});
  return {
    serialize,
    settle(id, ok, payload) {
      const entry = pending[id];
      if (!entry) throw new ErrorClass('Unknown host reply');
      delete pending[id];
      if (ok) entry.resolve(parse(payload));
      else entry.reject(new ErrorClass(payload));
    },
  };
})()
