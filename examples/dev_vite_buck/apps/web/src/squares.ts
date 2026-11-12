import { useEffect, useState } from "react";
import SquareWorker from "./square.worker.ts?worker";

const worker = new SquareWorker();

export function useSquares(n: number): number | null {
  const [result, setResult] = useState<number | null>(null);
  useEffect(() => {
    const onMessage = (e: MessageEvent<number>) => setResult(e.data);
    worker.addEventListener("message", onMessage);
    worker.postMessage(n);
    return () => worker.removeEventListener("message", onMessage);
  }, [n]);
  return result;
}
