import type { ReactNode } from "react";
import styles from "./Button.module.css";

export function Button({ children, onClick }: { children: ReactNode; onClick?: () => void }) {
  return (
    <button className={styles.button} onClick={onClick}>
      {children}
    </button>
  );
}
