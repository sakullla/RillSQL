export interface CompletionSnippet {
  label: string;
  detail: string;
  body: string;
  /** 展开后光标相对 body 起点的偏移。 */
  caret: number;
}

const SNIPPETS: CompletionSnippet[] = [
  { label: "select-from", detail: "SELECT … FROM …", body: "SELECT  FROM ", caret: 7 },
  { label: "join-on", detail: "JOIN … ON …", body: "JOIN  ON ", caret: 5 },
  { label: "insert-values", detail: "INSERT INTO … VALUES (…)", body: "INSERT INTO  () VALUES ()", caret: 12 },
  { label: "update-set", detail: "UPDATE … SET … WHERE …", body: "UPDATE  SET  WHERE ", caret: 7 },
  { label: "create-index", detail: "CREATE INDEX … ON …", body: "CREATE INDEX  ON  ()", caret: 13 },
  { label: "alter-table", detail: "ALTER TABLE … ADD COLUMN …", body: "ALTER TABLE  ADD COLUMN ", caret: 12 },
];

export function snippetCandidates(prefix: string): CompletionSnippet[] {
  const needle = prefix.toLowerCase();
  if (!needle) return SNIPPETS;
  return SNIPPETS.filter((item) => item.label.toLowerCase().startsWith(needle));
}
