import { useEffect, useState } from "react";

/** A counter that goes up every `ms`. Panels put it in their read effect so balances and
 *  "ready at" times update on their own, without reconnecting. */
export function useTick(ms = 10_000): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), ms);
    return () => clearInterval(id);
  }, [ms]);
  return tick;
}
