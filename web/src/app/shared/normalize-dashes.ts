/**
 * Replace em/en dashes with plain punctuation, deliberately mirroring the Rust
 * `crates/aarg-domain/src/tailor.rs::normalize_dashes` semantics EXACTLY.
 *
 * The domain scrubs résumé prose upstream, so the rendered payload is already
 * dash-free. But model-borne meta-text that never passes through that scrub —
 * reviewer persona notes, objection messages/suggestions, interview prompts and
 * offered wordings, progress and notify strings — reaches the view raw. This
 * synchronous render path can't `await` the wasm `normalize_dashes` export, so
 * this is a knowing, pointed duplication of the Rust function, kept in lockstep
 * with it. Punctuation only: it never touches a number, name, or claim.
 *
 * A dash between two digits becomes a hyphen (a numeric range, `2020–2023` →
 * `2020-2023`); used as a clause separator it becomes a comma (surrounding
 * spaces collapse to the comma's single trailing space); at a string edge it's
 * dropped.
 */
export function normalizeDashes(text: string): string {
  // `Array.from` splits on code points, matching Rust's `chars()` over surrogate
  // pairs rather than UTF-16 code units.
  const chars = Array.from(text);
  let out = '';
  let i = 0;
  while (i < chars.length) {
    const c = chars[i];
    if (c === '—' || c === '–') {
      // The last non-space output char and the next non-space input char decide
      // whether this dash is a numeric range or a clause break.
      let j = i + 1;
      while (j < chars.length && chars[j] === ' ') j += 1;
      const next = j < chars.length ? chars[j] : undefined;
      const trimmed = out.replace(/\s+$/, '');
      const prev = trimmed.length > 0 ? trimmed[trimmed.length - 1] : undefined;
      // Mirror the Rust exactly: read `prev` off a whitespace-trimmed view, but
      // only pop trailing *spaces* from the actual buffer.
      while (out.endsWith(' ')) out = out.slice(0, -1);
      if (prev !== undefined && next !== undefined && isAsciiDigit(prev) && isAsciiDigit(next)) {
        out += '-';
      } else if (prev !== undefined && next !== undefined) {
        // Don't double up if the clause already closes with punctuation.
        if (!',;:.!?'.includes(prev)) out += ',';
        out += ' ';
      }
      // A dash with nothing on one side is just dropped.
      i = j;
    } else {
      out += c;
      i += 1;
    }
  }
  return out;
}

function isAsciiDigit(ch: string): boolean {
  return ch >= '0' && ch <= '9';
}
