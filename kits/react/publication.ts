import { createContext } from "react";

/** Separate runtime readiness from committed route-loader ownership. */
export const SessionPublication = createContext<{
  getSnapshot(): number | null;
  subscribe(listener: () => void): () => void;
}>({ getSnapshot: () => null, subscribe: () => () => {} });
