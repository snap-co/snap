/** React escapes source text; only explicit HTTP(S) URLs become navigable links. */
export function Text({ value }: { value: string }) {
  return <>{value.split(/(```[\s\S]*?```)/g).map((part, i) => part.startsWith("```") ? <pre key={i}><code>{part.replace(/^```[^\n]*\n?/, "").replace(/```$/, "")}</code></pre> : part.split(/\n\n+/).filter(Boolean).map((paragraph, j) => <p key={`${i}-${j}`}>{paragraph.split(/(\[[^\]]+\]\(https?:\/\/[^\s)]+\)|https?:\/\/[^\s<>]+)/g).map((piece, k) => {
    const link = piece.match(/^\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)$/);
    if (link) return <a key={k} href={link[2]} target="_blank" rel="noopener noreferrer">{link[1]}</a>;
    if (/^https?:\/\//.test(piece)) { const url = piece.replace(/[.,;!?)]+$/, ""); return <span key={k}><a href={url} target="_blank" rel="noopener noreferrer">{url}</a>{piece.slice(url.length)}</span>; }
    return piece;
  })}</p>))}</>;
}
