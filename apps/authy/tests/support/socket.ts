// Bun supports request headers on its global WebSocket constructor. The combined
// browser/Bun typecheck selects DOM's constructor, so expose Bun's extra argument.
export const Socket = globalThis.WebSocket as unknown as {
  new(url: string | URL, options: Bun.WebSocketOptions): WebSocket;
};
