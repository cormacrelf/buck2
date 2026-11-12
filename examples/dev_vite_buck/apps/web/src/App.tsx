import { useState } from "react";
import { Button } from "@demo/ui";
import { formatBytes } from "@demo/format";
import styles from "./App.module.css";
import logoUrl from "./logo.svg";
import logoSource from "./logo.svg?raw";
import data from "./data.json";
import { release } from "./build_info";
import { Notes } from "./Notes";
import { useSquares } from "./squares";

export function App() {
  const [count, setCount] = useState(0);
  const squares = useSquares(count);
  return (
    <main className={styles.app}>
      <header className={styles.header}>
        <img src={logoUrl} alt="" width={48} height={48} />
        <h1>{data.title}</h1>
      </header>
      <p>{import.meta.env.VITE_GREETING}</p>
      <p>
        Release {release.version} ({release.channel})
      </p>
      <Button onClick={() => setCount((c) => c + 1)}>Clicked {count} times</Button>
      <p>
        {count} kilobytes is {formatBytes(count * 1024)}; squared in a worker: {squares ?? "…"}
      </p>
      <p className={styles.small}>The logo is {logoSource.length} bytes of SVG.</p>
      <Notes />
    </main>
  );
}
