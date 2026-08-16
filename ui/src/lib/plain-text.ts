/**
 * Markdown with its markup removed, for the speech synthesiser.
 *
 * # Why this exists
 *
 * A typed question gets a written answer, and a written answer is markdown —
 * the same string `MarkdownContent` renders. Handed to a voice unchanged, the
 * voice reads it: "asterisco asterisco DynamoDB asterisco asterisco". So the
 * *Ouvir* button on an answer card strips it first.
 *
 * # Why here and not in the Rust `speak` command
 *
 * That command serves callers whose text never was markdown — a spoken answer,
 * a dictation confirmation. Turning it into a stripper would make a literal
 * asterisk unspeakable everywhere, to fix a problem that belongs to one button.
 *
 * # Why it is deliberately crude
 *
 * It only ever runs on text this app's own prompt produced, and that prompt asks
 * for headings, emphasis, bullets and the occasional link. A stray character
 * read aloud is a smaller failure than a half-parsed sentence, so every rule
 * here degrades toward "leave the text alone".
 */
export function plainText(markdown: string): string {
  return (
    markdown
      // Fences first: their contents must not be mistaken for emphasis or
      // bullets, and code is not worth reading aloud in the first place.
      .replace(/```[\s\S]*?```/g, " ")
      .replace(/`([^`]*)`/g, "$1")
      // Links keep the label and drop the URL, which no one wants dictated.
      .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/^\s{0,3}#{1,6}\s+/gm, "")
      .replace(/^\s{0,3}>\s?/gm, "")
      .replace(/^\s*[-*+]\s+/gm, "")
      .replace(/^\s*\d+\.\s+/gm, "")
      // Doubles before singles, or `**x**` loses one pair and keeps the other.
      .replace(/(\*\*|__)(.*?)\1/g, "$2")
      .replace(/(\*|_)(.*?)\1/g, "$2")
      // A blank line was a pause on the page; it becomes one for the voice.
      .replace(/\n{2,}/g, ". ")
      .replace(/\s+/g, " ")
      // A removed block leaves the pause on both sides of it. Two full stops in
      // a row is a stumble the voice actually performs.
      .replace(/\.\s+\./g, ".")
      .trim()
  );
}
