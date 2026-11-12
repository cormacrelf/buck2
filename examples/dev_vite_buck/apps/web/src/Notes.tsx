const notes = import.meta.glob("./notes/*.md", { eager: true, query: "?raw", import: "default" }) as Record<string, string>;

export function Notes() {
  return (
    <ul>
      {Object.entries(notes).map(([path, text]) => (
        <li key={path}>
          <code>{path}</code>: {text.trim()}
        </li>
      ))}
    </ul>
  );
}
