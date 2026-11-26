// Anything the harness doesn't exercise (stores, IPC, embeds): a callable proxy all the way down.
const handler = { get: (_t, k) => (k === "__esModule" ? false : k === Symbol.toPrimitive ? () => "" : any), apply: () => any };
const any = new Proxy(function () {}, handler);
module.exports = any;
