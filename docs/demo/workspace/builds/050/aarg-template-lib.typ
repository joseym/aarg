// aarg shared Typst helpers — imported with `#import "aarg-template-lib.typ": *`.
//
// The renderer stages this file next to each template in the build dir, so
// templates import it as a bare filename (not a path). Everything here is a
// pure helper: date formatting, defensive field access, and small layout
// primitives that more than one template can reuse.

// "2020-03" -> "Mar 2020"; none -> "Present".
// Used for every start/end date so current roles read as "Present".
#let fmt(ym) = {
  if ym == none { "Present" } else {
    let parts = ym.split("-")
    let months = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
                  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
    months.at(int(parts.at(1)) - 1) + " " + parts.at(0)
  }
}

// Render a "start -- end" date range, e.g. "Jan 2020 -- Present".
#let date_range(start, end) = fmt(start) + " -- " + fmt(end)

// Defensive optional read: returns data.key if the key exists and is non-none,
// otherwise `default`. Keeps templates free of repeated `"k" in data and ...`.
#let field(data, key, default: none) = {
  if key in data and data.at(key) != none { data.at(key) } else { default }
}

// A "#rrggbb" string -> a Typst color; null/invalid -> `fallback`.
// Lets templates honor layout_hints.accent_color without crashing on null.
#let parse_color(value, fallback: rgb("#1f6feb")) = {
  if value == none { fallback } else { rgb(value) }
}

// A bolded section heading with an accent underline. The accent color is
// passed in so each template controls its own palette.
#let heading_rule(title, accent) = block(above: 13pt, below: 7pt, breakable: false)[
  #text(size: 11pt, weight: "bold", fill: accent, tracking: 0.5pt)[#upper(title)]
  #v(-7pt)
  #line(length: 100%, stroke: 0.8pt + accent)
]

// A lighter section heading for a tinted sidebar column (no rule line — the
// column edge already separates content, and a rule would feel heavy).
#let sidebar_heading(title, accent) = block(above: 12pt, below: 5pt, breakable: false)[
  #text(size: 10pt, weight: "bold", fill: accent, tracking: 0.6pt)[#upper(title)]
]
