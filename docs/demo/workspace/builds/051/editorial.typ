// aarg human template "editorial": a polished, magazine-like two-column resume
// for human readers. A serif display name, an all-caps target title, and a
// monochrome grayscale palette evoke a print editorial layout — while the text
// stays real and selectable (no images-of-text) so the PDF still parses.
//
// Contract: compiled with `--input data=<path>` pointing at the human-variant
// payload JSON (the canonical resume plus `layout_hints`). The shared lib is
// staged beside this file by the renderer, hence the bare import.
//
// This layout is inherently two-column, so it always renders a sidebar
// regardless of the `sidebar` hint. It is intentionally MONOCHROME: the
// `accent_color` hint is ignored to keep the editorial grayscale look.

#import "aarg-template-lib.typ": *

#let data = json(sys.inputs.data)

// ----- layout_hints (read defensively; payload may omit the block) -----
#let hints = field(data, "layout_hints", default: (:))
#let show_summary = field(hints, "show_summary", default: true) == true
#let density = field(hints, "density", default: "standard")
#let max_pages = field(hints, "max_pages", default: 2)

// ----- monochrome palette -----
// Near-black primary ink, a medium gray for secondary text, plus the page wash.
#let ink = rgb("#1b1b1b")
#let muted = rgb("#6e6e6e")
#let hairline = rgb("#9a9a9a")
#let page_bg = rgb("#efefef")

// ----- font stacks -----
// A serif display face for the name; a clean sans for everything else. The
// DejaVu fallbacks ship with typst, so missing-font warnings are harmless.
#let serif_face = ("Playfair Display", "Didot", "Bodoni 72", "Georgia", "DejaVu Serif")
#let sans_face = ("Helvetica Neue", "Inter", "Arial", "DejaVu Sans")

// Density tuning: one knob drives body size, line leading, and the gaps between
// blocks/list items so "compact" reads tight and "airy" breathes.
#let body_size = if density == "compact" { 9pt } else if density == "airy" { 10pt } else { 9.5pt }
#let leading = if density == "compact" { 0.55em } else if density == "airy" { 0.7em } else { 0.62em }
#let block_gap = if density == "compact" { 8pt } else if density == "airy" { 14pt } else { 11pt }
#let sec_gap = if density == "compact" { 13pt } else if density == "airy" { 22pt } else { 17pt }
#let list_gap = if density == "compact" { 0.4em } else if density == "airy" { 0.6em } else { 0.5em }

// ----- global page + text defaults -----
#set document(title: field(data.contact, "full_name", default: "resume") + " - resume")
#set page(paper: "us-letter", margin: (x: 0.6in, y: 0.5in), fill: page_bg)
#set text(font: sans_face, size: body_size, fill: muted)
#set par(justify: false, leading: leading)
#set list(spacing: list_gap, marker: text(fill: ink)[#sym.bullet], indent: 0pt, body-indent: 6pt)

// ===== reusable content pieces =====

// A bold all-caps sans section heading with wide tracking. Used in both columns.
#let section_head(title) = block(above: sec_gap, below: 6pt, breakable: false)[
  #text(font: sans_face, size: 14pt, weight: "bold", fill: ink, tracking: 0.18em)[#upper(title)]
]

// A small bold caps subsection label (e.g. "TECHNICAL").
#let sub_label(title) = block(above: 2pt, below: 4pt, breakable: false)[
  #text(font: sans_face, size: 10pt, weight: "bold", fill: ink, tracking: 0.05em)[#upper(title)]
]

// The masthead: a short decorative hairline, then the name split across two
// lines (first token small, the rest large + bold), then the target title.
#let masthead = {
  let name = field(data.contact, "full_name", default: "")
  // Decorative hairline above the name.
  align(center, line(angle: 90deg, length: 0.55in, stroke: 0.6pt + hairline))
  v(6pt)
  // Split the name on whitespace: first token small, remainder large + bold.
  let tokens = name.split(regex("\s+")).filter(t => t != "")
  set par(leading: 0.2em)
  if tokens.len() <= 1 {
    align(center, text(
      font: serif_face, size: 48pt, weight: "bold", fill: ink, tracking: 0.28em,
    )[#upper(name)])
  } else {
    let first = tokens.first()
    let rest = tokens.slice(1).join(" ")
    // Both name lines in ONE centered paragraph so `par.leading` (set above)
    // controls the gap between them. As two separate `align` blocks they ignored
    // the leading and fell back to the much larger inter-block spacing.
    align(center, {
      text(
        font: serif_face, size: 22pt, weight: "regular", fill: ink, tracking: 0.3em,
      )[#upper(first)]
      linebreak()
      text(
        font: serif_face, size: 48pt, weight: "bold", fill: ink, tracking: 0.28em,
      )[#upper(rest)]
    })
  }
  let tt = field(data, "target_title")
  if tt != none {
    v(8pt)
    align(center, text(
      font: sans_face, size: 12pt, weight: "regular", fill: muted, tracking: 0.4em,
    )[#upper(tt)])
  }
}

// ===== left column sections =====

// CONTACT: email, phone, location, then each link — one per line, small.
#let contact_section = {
  let c = data.contact
  let lines = ()
  let email = field(c, "email")
  if email != none { lines.push(email) }
  let phone = field(c, "phone")
  if phone != none { lines.push(phone) }
  let loc = field(c, "location")
  if loc != none { lines.push(loc) }
  for l in field(c, "links", default: ()) { lines.push(link(l.url)[#l.label]) }
  if lines.len() > 0 {
    section_head("Contact")
    set text(size: body_size - 0.5pt, fill: muted)
    lines.map(l => [#l]).join(linebreak())
  }
}

// EXPERTISE: a "TECHNICAL" subsection holding the skills as a bulleted list.
#let expertise_section = {
  let groups = field(data, "skill_groups", default: ())
  let skills = field(data, "skills_section", default: (skills: ())).skills
  if groups.len() > 0 {
    // Curated, grouped skills (human variant): one sub-label per group, with a
    // gap before each group after the first so they read as distinct clusters
    // rather than one dense list.
    section_head("Expertise")
    for (i, g) in groups.enumerate() {
      if i > 0 { v(block_gap) }
      sub_label(g.label)
      for s in g.skills [ - #s ]
    }
  } else if skills.len() > 0 {
    // Fallback: a flat list under a single "Technical" label.
    section_head("Expertise")
    sub_label("Technical")
    for s in skills [ - #s ]
  }
}

// EDUCATION: institution, credential, and (if known) the date range.
#let education_section = {
  let entries = field(data, "education", default: ())
  if entries.len() > 0 {
    section_head("Education")
    for e in entries {
      block(above: block_gap - 3pt, below: 3pt, breakable: false)[
        #text(weight: "bold", fill: ink)[#e.institution]
        #let cred = field(e, "credential", default: "")
        #if cred != "" [ #linebreak() #text(fill: muted)[#cred] ]
        #let s = field(e, "start")
        #let en = field(e, "end")
        #if en != none [
          #linebreak()
          #text(size: body_size - 0.5pt, fill: muted)[#date_range(s, en)]
        ]
      ]
    }
  }
}

// CERTIFICATIONS: each name, issuer, and (if known) the issue date.
#let certifications_section = {
  let certs = field(data, "certifications", default: ())
  if certs.len() > 0 {
    section_head("Certifications")
    for c in certs [
      - #text(weight: "medium", fill: ink)[#c.name]#{
          let issuer = field(c, "issuer", default: "")
          if issuer != "" [ #text(fill: muted)[ · #issuer]]
        }#{
          let issued = field(c, "issued")
          if issued != none [ #text(size: body_size - 0.5pt, fill: muted)[ (#fmt(issued))]]
        }
    ]
  }
}

// ===== right column sections =====

// PROFILE: the summary paragraph, gated on show_summary + non-empty text.
#let profile_section = {
  let s = field(data, "summary", default: "")
  if show_summary and s != "" {
    section_head("Profile")
    text(fill: muted)[#s]
  }
}

// One experience entry: title (bold, uppercased), a "Company  |  dates" line,
// then the role's bullets as a dotted list. Breakable on purpose: a long role
// (the recent one can carry six bullets) flows across a page break rather than
// jumping wholesale to the next page, which would strand the "EXPERIENCE"
// heading at the foot of page one and start the roles on page two.
#let role_entry(role) = block(above: block_gap, below: 3pt, breakable: true)[
  #text(font: sans_face, size: 11pt, weight: "bold", fill: ink, tracking: 0.04em)[#upper(role.title)]
  #linebreak()
  #{
    let parts = (role.company,)
    let loc = field(role, "location")
    let meta = text(
      font: sans_face, size: body_size - 0.5pt, fill: muted, tracking: 0.08em,
    )[#role.company#h(6pt)|#h(6pt)#date_range(role.start, field(role, "end"))]
    meta
    if loc != none [
      #linebreak()
      #text(size: body_size - 1pt, fill: muted, style: "italic")[#loc]
    ]
  }
  #v(2pt)
  #for b in field(role, "bullets", default: ()) [ - #b.text ]
]

// EXPERIENCE: every role, in payload order.
#let experience_section = {
  let roles = field(data, "roles", default: ())
  if roles.len() > 0 {
    section_head("Experience")
    for role in roles { role_entry(role) }
  }
}

// PROJECTS: each project styled like an experience entry (name + summary).
#let projects_section = {
  let projects = field(data, "projects", default: ())
  if projects.len() > 0 {
    section_head("Projects")
    for p in projects {
      block(above: block_gap, below: 3pt, breakable: false)[
        #text(font: sans_face, size: 11pt, weight: "bold", fill: ink, tracking: 0.04em)[#upper(p.name)]
        #let url = field(p, "url")
        #if url != none [ #h(6pt)#text(size: body_size - 1pt, fill: muted)[#link(url)[#url]]]
        #let sum = field(p, "summary", default: "")
        #if sum != "" [
          #v(2pt)
          #text(fill: muted)[#sum]
        ]
      ]
    }
  }
}

// ACHIEVEMENTS: reusable wins (awards, talks, open source), one per bullet.
#let achievements_section = {
  let items = field(data, "achievements", default: ())
  if items.len() > 0 {
    section_head("Achievements")
    for a in items [ - #text(fill: muted)[#a.text] ]
  }
}

// ===== assemble the page =====

// Full-width centered masthead above the two columns.
#masthead
#v(block_gap)

// Two columns split by a thin vertical rule. A stroked grid draws the divider
// only as tall as the content (no full-height rect, which would force the row
// to a whole page). Left ~32%, comfortable gutter, then the wider right column.
#grid(
  columns: (33%, 1fr),
  // Pad both columns away from the divider: a right-inset on the left cell and
  // a left-inset on the right cell put equal breathing room on each side of the
  // rule, instead of a column-gutter (which only padded the left column and let
  // the right column's text sit flush against the line).
  inset: (x, y) => if x == 0 { (right: 18pt) } else { (left: 18pt) },
  stroke: (x, y) => if x == 1 { (left: 0.6pt + hairline) } else { none },
  // --- left column ---
  [
    #contact_section
    #expertise_section
    #education_section
    #certifications_section
  ],
  // --- right column ---
  [
    #profile_section
    #experience_section
    #projects_section
    #achievements_section
  ],
)
