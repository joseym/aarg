// aarg human template "modern": a visually-designed, recruiter-scannable
// resume for human readers. Stronger hierarchy than the ATS variant — accent
// color, a bold name/title header, optional left sidebar — while staying real,
// selectable text (no images-of-text, no rasterization) so the PDF parses.
//
// Contract: compiled with `--input data=<path>` pointing at the human-variant
// payload JSON (a superset of the canonical resume plus `layout_hints`). The
// shared lib is staged beside this file by the renderer, hence the bare import.

#import "aarg-template-lib.typ": *

#let data = json(sys.inputs.data)

// ----- layout_hints (read defensively; payload may omit the block) -----
#let hints = field(data, "layout_hints", default: (:))
#let accent = parse_color(field(hints, "accent_color"))
#let use_sidebar = field(hints, "sidebar", default: false) == true
#let show_summary = field(hints, "show_summary", default: true) == true
#let density = field(hints, "density", default: "standard")
#let max_pages = field(hints, "max_pages", default: 2)

// Density tuning: one knob drives body size, line leading, and the gaps
// between blocks/list items so "compact" reads tight and "airy" breathes.
#let body_size = if density == "compact" { 9.5pt } else if density == "airy" { 10.75pt } else { 10pt }
#let leading = if density == "compact" { 0.52em } else if density == "airy" { 0.7em } else { 0.6em }
#let block_gap = if density == "compact" { 7pt } else if density == "airy" { 12pt } else { 9pt }
#let list_gap = if density == "compact" { 0.4em } else if density == "airy" { 0.6em } else { 0.5em }

// A muted ink for secondary text (locations, dates, issuers).
#let muted = luma(95)

// ----- global page + text defaults -----
#set document(title: data.contact.full_name + " - resume")
#set page(paper: "us-letter", margin: (x: 0.65in, y: 0.6in))
#set text(size: body_size, fill: luma(25))
#set par(justify: false, leading: leading)
#set list(spacing: list_gap, marker: text(fill: accent)[•])

// ===== reusable content pieces =====

// The masthead: name (large, bold) + optional target title in the accent color.
// Shared by both single-column and sidebar layouts.
#let masthead = {
  text(size: 22pt, weight: "bold", fill: luma(15))[#data.contact.full_name]
  let tt = field(data, "target_title")
  if tt != none {
    v(2pt)
    text(size: 12.5pt, weight: "medium", fill: accent)[#tt]
  }
}

// Contact bits as a single flowing line (used in the single-column header).
#let contact_inline = {
  let bits = (data.contact.email, data.contact.phone, data.contact.location)
    .filter(x => x != none)
  let links = data.contact.links.map(l => link(l.url)[#l.label])
  text(size: 9pt, fill: muted)[#(bits + links).join("  ·  ")]
}

// Contact bits stacked vertically (used in the sidebar column).
#let contact_stacked = {
  set text(size: 9pt, fill: luma(45))
  let lines = ()
  if data.contact.email != none { lines.push(data.contact.email) }
  if data.contact.phone != none { lines.push(data.contact.phone) }
  if data.contact.location != none { lines.push(data.contact.location) }
  for l in data.contact.links { lines.push(link(l.url)[#l.label]) }
  lines.map(l => [#l]).join(linebreak())
}

// One experience entry: bold role + company, right-aligned dates, location,
// then accent-marked bullets. The grid keeps the date column aligned.
#let role_entry(role) = block(above: block_gap, below: 3pt, breakable: false)[
  #grid(
    columns: (1fr, auto),
    column-gutter: 10pt,
    [
      #text(weight: "bold", size: body_size + 0.5pt)[#role.title]
      #text(fill: accent, weight: "medium")[ · #role.company]
      #if role.location != none [
        #linebreak()
        #text(size: 8.5pt, fill: muted, style: "italic")[#role.location]
      ]
    ],
    text(size: 9pt, fill: muted)[#date_range(role.start, role.end)],
  )
  #for b in role.bullets [
    - #b.text
  ]
]

// ===== section renderers =====
// In the sidebar layout, skills/contact live in the left column and use the
// lighter `sidebar_heading`; everything else uses the ruled `heading_rule`.

#let summary_section = {
  let s = field(data, "summary", default: "")
  if show_summary and s != "" {
    heading_rule("Summary", accent)
    text(fill: luma(40))[#s]
  }
}

#let experience_section = {
  if data.roles.len() > 0 {
    heading_rule("Experience", accent)
    for role in data.roles { role_entry(role) }
  }
}

#let projects_section = {
  if data.projects.len() > 0 {
    heading_rule("Projects", accent)
    for p in data.projects {
      block(above: block_gap - 2pt, below: 2pt, breakable: false)[
        #text(weight: "bold")[#p.name]
        #if p.url != none [ #text(size: 8.5pt, fill: muted)[(#link(p.url)[#p.url])]]
        #linebreak()
        #text(fill: luma(40))[#p.summary]
      ]
    }
  }
}

#let education_section = {
  if data.education.len() > 0 {
    heading_rule("Education", accent)
    for e in data.education {
      block(above: 5pt, below: 3pt, breakable: false)[
        #grid(
          columns: (1fr, auto),
          column-gutter: 10pt,
          [
            #text(weight: "bold")[#e.institution]
            #if e.credential != "" [#linebreak() #text(size: 9pt, fill: luma(50))[#e.credential]]
          ],
          if e.end != none { text(size: 9pt, fill: muted)[#date_range(e.start, e.end)] },
        )
      ]
    }
  }
}

#let certifications_section = {
  if data.certifications.len() > 0 {
    heading_rule("Certifications", accent)
    for c in data.certifications [
      - #text(weight: "medium")[#c.name]#if c.issuer != "" [ #text(fill: muted)[— #c.issuer]]#if c.issued != none [ #text(size: 9pt, fill: muted)[(#fmt(c.issued))]]
    ]
  }
}

// Skills: chip-like inline list separated by accent dots. Rendered in the
// sidebar when present, otherwise as a normal section.
#let skills_body = {
  let skills = field(data, "skills_section", default: (skills: ())).skills
  skills.map(s => box(inset: (y: 1pt))[#s]).join(text(fill: accent)[ · ])
}
#let has_skills = field(data, "skills_section", default: (skills: ())).skills.len() > 0

// ===== assemble the page =====

#if use_sidebar {
  // Two-column: tinted left rail holds contact + skills; main column holds the
  // narrative. The name/title header spans the full width above both columns.
  masthead
  v(8pt)
  line(length: 100%, stroke: 0.8pt + accent)
  v(10pt)

  grid(
    columns: (1.9in, 1fr),
    column-gutter: 22pt,
    // --- left sidebar ---
    block(
      fill: accent.lighten(92%),
      inset: 11pt,
      radius: 3pt,
      width: 100%,
    )[
      #sidebar_heading("Contact", accent)
      #contact_stacked
      #if has_skills [
        #sidebar_heading("Skills", accent)
        #set text(size: 9pt)
        #set par(leading: 0.55em)
        #skills_body
      ]
    ],
    // --- main column ---
    [
      #summary_section
      #experience_section
      #projects_section
      #education_section
      #certifications_section
    ],
  )
} else {
  // Single-column: centered masthead, inline contact line, then sections.
  // Skills become a normal section since there is no sidebar to host them.
  align(center)[#masthead]
  v(5pt)
  align(center)[#contact_inline]
  v(2pt)

  summary_section
  experience_section
  if has_skills {
    heading_rule("Skills", accent)
    skills_body
  }
  projects_section
  education_section
  certifications_section
}
