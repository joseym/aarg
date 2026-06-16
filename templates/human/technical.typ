// aarg human template "technical": an engineering-forward, skills-first resume
// for human readers (email / in-person), NOT for ATS parsers — so it takes
// design liberties (monospace accents, chip-style skill tags, a tinted code-rail
// sidebar). Text stays real and selectable; nothing is rasterized.
//
// Contract: compiled with `--input data=<path>` pointing at the human-variant
// payload JSON (the canonical resume fields plus `layout_hints`). The shared lib
// is staged beside this file by the renderer, hence the bare import.

#import "aarg-template-lib.typ": *

#let data = json(sys.inputs.data)

// ----- layout_hints (read defensively; payload may omit the block) -----
#let hints = field(data, "layout_hints", default: (:))
#let accent = parse_color(field(hints, "accent_color"), fallback: rgb("#2f6f4f"))
#let use_sidebar = field(hints, "sidebar", default: false) == true
#let show_summary = field(hints, "show_summary", default: true) == true
#let density = field(hints, "density", default: "standard")
#let max_pages = field(hints, "max_pages", default: 2)

// Density tuning: one knob drives body size, line leading, and the gaps between
// blocks/list items so "compact" reads tight and "airy" breathes.
#let body_size = if density == "compact" { 9.5pt } else if density == "airy" { 10.75pt } else { 10pt }
#let leading = if density == "compact" { 0.55em } else if density == "airy" { 0.74em } else { 0.64em }
#let block_gap = if density == "compact" { 7pt } else if density == "airy" { 12pt } else { 9pt }
#let list_gap = if density == "compact" { 0.42em } else if density == "airy" { 0.62em } else { 0.52em }

// Palette: a near-black ink for body, a soft tint of the accent for fills, and a
// muted grey for secondary text (dates, locations, issuers).
#let ink = luma(28)
#let muted = luma(105)
#let tint = accent.lighten(90%)

// The monospace "code" voice used for section labels, the name mark, role meta,
// and skill chips — the one stylistic move that gives this template its feel.
// DejaVu Sans Mono ships with Typst; the rest are graceful fallbacks.
#let mono(body) = text(font: ("DejaVu Sans Mono", "Noto Sans Mono", "Nimbus Mono PS"), body)

// ----- global page + text defaults -----
// Body font is left to Typst's default (a sans) so the page renders the same on
// any machine; the monospace voice is applied selectively via `mono`.
#set document(title: data.contact.full_name + " - resume")
#set page(paper: "us-letter", margin: (x: 0.62in, y: 0.58in))
#set text(size: body_size, fill: ink)
#set par(justify: false, leading: leading)
#set list(spacing: list_gap, marker: mono(text(fill: accent, size: 0.95em)[$chevron.r$])) // a small ">" feel

// ===== reusable content pieces =====

// A monospace section label. Distinct from the lib's `heading_rule` so the
// technical voice carries through every heading; the rule is a thin double-tick
// of accent on the left to read like a terminal prompt rather than an underline.
#let tech_heading(title) = block(above: 13pt, below: 7pt, breakable: false)[
  #grid(
    columns: (auto, 1fr),
    column-gutter: 8pt,
    align: (left + horizon, left + horizon),
    mono(text(size: 9.5pt, weight: "bold", fill: accent, tracking: 1pt)[#upper(title)]),
    box(height: 1.2pt, width: 100%, fill: accent.lighten(55%)),
  )
]

// A lighter heading for the tinted sidebar column (no rule — the rail edge and
// fill already separate it; a rule would feel heavy in a narrow column).
#let rail_heading(title) = block(above: 12pt, below: 5pt, breakable: false)[
  #mono(text(size: 9pt, weight: "bold", fill: accent, tracking: 1pt)[#upper(title)])
]

// The masthead: name set in the mono voice (large, bold) with a thin accent
// keyline beneath, plus an optional target title as a subdued subtitle. Shared
// by both the single-column and sidebar layouts.
#let masthead = {
  mono(text(size: 21pt, weight: "bold", fill: luma(15))[#data.contact.full_name])
  let tt = field(data, "target_title")
  if tt != none {
    v(2.5pt)
    mono(text(size: 11pt, weight: "medium", fill: accent)[#upper(tt)])
  }
}

// Contact bits as a single flowing mono line (single-column header).
#let contact_inline = {
  let bits = (data.contact.email, data.contact.phone, data.contact.location)
    .filter(x => x != none)
  let links = data.contact.links.map(l => link(l.url)[#l.label])
  mono(text(size: 8.5pt, fill: muted)[#(bits + links).join(text(fill: accent)[ #h(2pt) / #h(2pt) ])])
}

// Contact bits stacked vertically (sidebar column).
#let contact_stacked = {
  set text(size: 8.5pt, fill: luma(55))
  let lines = ()
  if data.contact.email != none { lines.push(data.contact.email) }
  if data.contact.phone != none { lines.push(data.contact.phone) }
  if data.contact.location != none { lines.push(data.contact.location) }
  for l in data.contact.links { lines.push(link(l.url)[#l.label]) }
  mono(lines.map(l => [#l]).join(linebreak()))
}

// One experience entry: bold role + accent company, mono right-aligned dates,
// optional location, then accent-marked bullets. The grid keeps dates aligned.
// Breakable so a long role flows across a page break instead of bumping the
// whole Experience section to the next page.
#let role_entry(role) = block(above: block_gap, below: 3pt, breakable: true)[
  #grid(
    columns: (1fr, auto),
    column-gutter: 10pt,
    [
      #text(weight: "bold", size: body_size + 0.5pt)[#role.title]
      #mono(text(fill: accent, weight: "medium", size: body_size - 0.5pt)[ @ #role.company])
      #if ("location" in role) and role.location != none [
        #linebreak()
        #text(size: 8.5pt, fill: muted, style: "italic")[#role.location]
      ]
    ],
    mono(text(size: 8.5pt, fill: muted)[#date_range(role.start, role.end)]),
  )
  #for b in role.bullets [
    - #b.text
  ]
]

// Skills as monospace chips: each tag sits in a tinted, rounded box so the
// section reads as a scannable tech stack rather than a comma list. Each chip is
// built as an expression so the closure parameter stays in scope.
#let skill_chip(s, size) = box(
  fill: tint,
  inset: (x: 5pt, y: 2.5pt),
  radius: 2.5pt,
  outset: (y: 1.5pt),
  mono(text(size: size, fill: accent.darken(18%), s)),
)
#let skill_chips(size: 8.5pt) = {
  let groups = field(data, "skill_groups", default: ())
  let skills = field(data, "skills_section", default: (skills: ())).skills
  set par(leading: 0.9em)
  if groups.len() > 0 {
    // Curated, grouped skills (human variant): a mono label per group, then
    // that group's chips.
    for g in groups {
      block(above: 6pt, below: 1pt)[
        #mono(text(size: size - 1pt, fill: accent.darken(18%))[#upper(g.label)])
        #linebreak()
        #box(width: 100%, g.skills.map(s => skill_chip(s, size)).join(h(3pt)))
      ]
    }
  } else {
    box(width: 100%, skills.map(s => skill_chip(s, size)).join(h(3pt)))
  }
}
#let has_skills = field(data, "skills_section", default: (skills: ())).skills.len() > 0

// ===== section renderers =====

#let summary_section = {
  let s = field(data, "summary", default: "")
  if show_summary and s != "" {
    tech_heading("Summary")
    text(fill: luma(45))[#s]
  }
}

#let experience_section = {
  if data.roles.len() > 0 {
    tech_heading("Experience")
    for role in data.roles { role_entry(role) }
  }
}

#let projects_section = {
  if data.projects.len() > 0 {
    tech_heading("Projects")
    for p in data.projects {
      block(above: block_gap - 2pt, below: 2pt, breakable: false)[
        #text(weight: "bold")[#p.name]
        #if ("url" in p) and p.url != none [ #mono(text(size: 8pt, fill: muted)[ #link(p.url)[#p.url]])]
        #linebreak()
        #text(fill: luma(45))[#p.summary]
      ]
    }
  }
}

#let education_section = {
  if data.education.len() > 0 {
    tech_heading("Education")
    for e in data.education {
      block(above: 5pt, below: 3pt, breakable: false)[
        #grid(
          columns: (1fr, auto),
          column-gutter: 10pt,
          [
            #text(weight: "bold")[#e.institution]
            #if e.credential != "" [#linebreak() #text(size: 9pt, fill: luma(55))[#e.credential]]
          ],
          if ("end" in e) and e.end != none {
            mono(text(size: 8.5pt, fill: muted)[#date_range(e.start, e.end)])
          },
        )
      ]
    }
  }
}

#let certifications_section = {
  if data.certifications.len() > 0 {
    tech_heading("Certifications")
    for c in data.certifications [
      - #text(weight: "medium")[#c.name]#if ("issuer" in c) and c.issuer != "" [ #text(fill: muted)[· #c.issuer]]#if ("issued" in c) and c.issued != none [ #mono(text(size: 8.5pt, fill: muted)[ (#fmt(c.issued))])]
    ]
  }
}

#let skills_section = {
  if has_skills {
    tech_heading("Skills")
    skill_chips()
  }
}

// ===== assemble the page =====

#if use_sidebar {
  // Two-column: a tinted "rail" on the left holds contact + skills (the at-a-
  // glance stack), the main column carries the narrative. The masthead spans the
  // full width above both columns.
  masthead
  v(7pt)
  line(length: 100%, stroke: (paint: accent, thickness: 1.4pt))
  v(10pt)

  grid(
    columns: (1.95in, 1fr),
    column-gutter: 20pt,
    // --- left rail ---
    block(
      fill: tint,
      inset: 11pt,
      radius: 3pt,
      width: 100%,
      stroke: (left: 2pt + accent),
    )[
      #rail_heading("Contact")
      #contact_stacked
      #if has_skills [
        #rail_heading("Stack")
        #skill_chips(size: 8pt)
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
  // Single-column: centered masthead, an inline mono contact line, a keyline,
  // then sections. Skills become a normal chip section (no rail to host them).
  align(center)[#masthead]
  v(5pt)
  align(center)[#contact_inline]
  v(6pt)
  line(length: 100%, stroke: (paint: accent.lighten(40%), thickness: 0.8pt))

  summary_section
  skills_section
  experience_section
  projects_section
  education_section
  certifications_section
}
