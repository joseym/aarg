/** Renders a chat message's markdown (bold, lists, links, inline code) to
 *  HTML for a chat bubble — used for both the advisor's reply (plain GFM
 *  prose, unprompted) and what the person typed. Angular's `[innerHTML]`
 *  binding sanitizes the result, so this only needs to produce well-formed
 *  HTML, not defend against injection itself. `breaks: true` turns a single
 *  typed newline (Shift+Enter in the compose box) into a `<br>`, matching
 *  what the bubble showed before markdown rendering replaced a plain
 *  `white-space: pre-wrap` text node. */

import { Marked } from 'marked';

const renderer = {
  // Every rendered link opens in a new tab without handing the target page a
  // `window.opener` back into this one (the same convention the resume
  // preview uses for project URLs).
  link({ href, title, text }: { href: string; title?: string | null; text: string }): string {
    const titleAttr = title ? ` title="${escapeAttr(title)}"` : '';
    return `<a href="${escapeAttr(href)}"${titleAttr} target="_blank" rel="noopener noreferrer">${text}</a>`;
  },
};

const marked = new Marked({ gfm: true, breaks: true });
marked.use({ renderer });

function escapeAttr(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/"/g, '&quot;');
}

/** `text` rendered to an HTML string. On a parse failure, falls back to the
 *  text with HTML metacharacters escaped, so it still lands correctly inside
 *  the `[innerHTML]` sink (as visible text, not misparsed markup) instead of
 *  dropping the bubble. */
export function renderChatMarkdown(text: string): string {
  try {
    const html = marked.parse(text, { async: false });
    return typeof html === 'string' ? html : escapeHtml(text);
  } catch {
    return escapeHtml(text);
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
