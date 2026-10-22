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
  const TypeErrorClass = TypeError;
  const ProxyClass = Proxy;
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
  // An unknown name fails with guidance instead of "not a function"; then/toJSON stay
  // undefined so awaiting or serializing the object behaves like a plain object.
  globalThis.tools = new ProxyClass(freeze(tools), {
    get(target, key) {
      if (typeof key !== 'string' || key in target || key === 'then' || key === 'toJSON') return target[key];
      throw new ErrorClass(`Unknown tool "${key}"; use catalog.search() to find tool names`);
    },
  });
  globalThis.catalog = freeze({
    search(query = '', offset = 0) {
      // Accept the common object form; reject other types instead of searching "[object Object]".
      if (query !== null && typeof query === 'object') ({query = '', offset = 0} = query);
      if (typeof query !== 'string')
        throw new TypeErrorClass('catalog.search(query, offset) takes a string query, e.g. catalog.search("issue")');
      if (!isSafeInteger(offset) || offset < 0) throw new ErrorClass('Invalid catalog offset');
      // Every whitespace-separated term must appear in the name or description.
      const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
      const matches = inventory.filter(t => {
        const text = `${t.name} ${String(t.description || '')}`.toLowerCase();
        return terms.every(term => text.includes(term));
      });
      return {total: matches.length, tools: matches.slice(offset, offset + 20).map(t => ({name: t.name, description: t.description}))};
    },
    describe(name) {
      if (name !== null && typeof name === 'object') ({name} = name);
      if (typeof name !== 'string')
        throw new TypeErrorClass('catalog.describe(name) takes a tool name string');
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
