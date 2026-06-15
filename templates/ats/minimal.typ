// aarg ATS template "minimal": an austere single-column layout built to
// survive resume parsers. Plainer than "classic" — no accent color, no rules
// beyond a single thin separator under the name, tighter whitespace — but the
// same content in the same document order.
//
// ATS parser-safety is the hard requirement: one column, normal top-to-bottom
// text flow, no multi-column grids/tables/graphics/headers/footers/icons, only
// selectable real text, and plain standard section headings.
//
// Contract: compiled with `--input data=<path>` pointing at the canonical
// tailored-resume JSON (relative to this file's directory). The renderer stages
// `aarg-template-lib.typ` next to this file, so it imports by bare filename.

#import "aarg-template-lib.typ": fmt, date_range, field

#let data = json(sys.inputs.data)

#set document(title: data.contact.full_name + " - resume")
#set page(paper: "us-letter", margin: (x: 0.7in, y: 0.6in))
// A common sans-serif stack: Arial/Helvetica on macOS/Windows, then the
// metric-compatible Liberation/DejaVu/Nimbus families found on Linux and CI, so
// the clean ATS look survives wherever it renders. Typst falls back gracefully
// if none are present.
#set text(
  font: ("Arial", "Helvetica", "Liberation Sans", "DejaVu Sans", "Nimbus Sans"),
  size: 10.5pt,
)
#set par(justify: false, leading: 0.5em)
#set list(spacing: 0.4em, indent: 0pt, body-indent: 0.5em)
#set block(spacing: 0.55em)

// A plain section heading: bold, slightly larger than body text, in normal
// document flow. No color, no rule — minimal chrome by design.
#let section(title) = block(above: 9pt, below: 4pt, breakable: false)[
  #text(size: 11pt, weight: "bold")[#upper(title)]
]

// ----- header: name, optional target title, then one contact line -----
#align(center)[
  #text(size: 16pt, weight: "bold")[#data.contact.full_name]
]

#let target = field(data, "target_title")
#if target != none [
  #align(center)[#v(2pt) #text(size: 11pt)[#target]]
]

#{
  // A single line of plain, comma-separated contact facts — parsers read this
  // far more reliably than icons or a multi-column block. Omit any that are
  // null/empty so partial payloads still render cleanly.
  let bits = (
    field(data.contact, "email"),
    field(data.contact, "phone"),
    field(data.contact, "location"),
  ).filter(x => x != none and x != "")
  let links = field(data.contact, "links", default: ()).map(l => l.url)
  let all = bits + links
  if all.len() > 0 {
    align(center)[#v(3pt) #text(size: 9.5pt)[#all.join("  •  ")]]
  }
}

#v(2pt)
#line(length: 100%, stroke: 0.5pt)

// ----- summary (honor the layout hint, fall back to non-empty text) -----
// Read layout_hints defensively: an older payload may omit it entirely.
#let layout_hints = field(data, "layout_hints", default: (:))
#let show_summary = field(layout_hints, "show_summary", default: true)
#let summary = field(data, "summary", default: "")
#if show_summary and summary != "" [
  #section("Summary")
  #summary
]

// ----- experience -----
#let roles = field(data, "roles", default: ())
#if roles.len() > 0 [
  #section("Experience")
  #for role in roles [
    #block(above: 7pt, below: 3pt, breakable: false)[
      // Title and company on one line; dates on the next so the whole entry
      // stays single-column top-to-bottom (no grid an ATS might misread).
      #text(weight: "bold")[#role.title]#{
        let company = field(role, "company")
        if company != none [ — #company]
      }#{
        let loc = field(role, "location")
        if loc != none [, #loc]
      } \
      #text(size: 9.5pt, style: "italic")[#date_range(field(role, "start"), field(role, "end"))]
    ]
    #let bullets = field(role, "bullets", default: ())
    #for b in bullets [
      - #b.text
    ]
  ]
]

// ----- skills -----
#let skills = field(field(data, "skills_section", default: (:)), "skills", default: ())
#if skills.len() > 0 [
  #section("Skills")
  #skills.join(", ")
]

// ----- education -----
#let education = field(data, "education", default: ())
#if education.len() > 0 [
  #section("Education")
  #for e in education [
    #block(above: 4pt, below: 2pt, breakable: false)[
      #text(weight: "bold")[#e.institution]#{
        let credential = field(e, "credential", default: "")
        if credential != "" [, #credential]
      }#{
        let end = field(e, "end")
        if end != none [ #h(1fr) #text(style: "italic")[#fmt(end)]]
      }
    ]
  ]
]

// ----- projects -----
#let projects = field(data, "projects", default: ())
#if projects.len() > 0 [
  #section("Projects")
  #for p in projects [
    #block(above: 5pt, below: 2pt, breakable: false)[
      #text(weight: "bold")[#p.name]#{
        let url = field(p, "url")
        if url != none [ (#url)]
      }
    ]
    #let summary = field(p, "summary", default: "")
    #if summary != "" [#summary]
  ]
]

// ----- certifications -----
#let certifications = field(data, "certifications", default: ())
#if certifications.len() > 0 [
  #section("Certifications")
  #for c in certifications [
    - #c.name#{
        let issuer = field(c, "issuer", default: "")
        if issuer != "" [, #issuer]
      }#{
        let issued = field(c, "issued")
        if issued != none [ (#fmt(issued))]
      }
  ]
]
