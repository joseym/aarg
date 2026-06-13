// aarg ATS template "classic": single column, standard headings,
// selectable text, no graphics — built to survive resume parsers.
//
// Contract: compiled with `--input data=<path>` pointing at the
// canonical tailored-resume JSON (relative to this file's directory).

#let data = json(sys.inputs.data)

#set document(title: data.contact.full_name + " - resume")
#set page(paper: "us-letter", margin: (x: 0.75in, y: 0.65in))
#set text(size: 10.5pt)
#set par(justify: false, leading: 0.5em)
#set list(spacing: 0.5em)

// "2020-03" -> "Mar 2020"; none -> "Present"
#let fmt(ym) = {
  if ym == none { "Present" } else {
    let parts = ym.split("-")
    let months = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
                  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
    months.at(int(parts.at(1)) - 1) + " " + parts.at(0)
  }
}

#let section(title) = block(above: 11pt, below: 7pt)[
  #text(size: 11.5pt, weight: "bold")[#upper(title)]
  #v(-8pt)
  #line(length: 100%, stroke: 0.5pt)
]

// ----- header -----
#align(center)[
  #text(size: 17pt, weight: "bold")[#data.contact.full_name]
]
#if "target_title" in data and data.target_title != none [
  #align(center)[#text(size: 12pt)[#data.target_title]]
]
#{
  let bits = (data.contact.email, data.contact.phone, data.contact.location)
    .filter(x => x != none)
  let links = data.contact.links.map(l => l.url)
  align(center)[#(bits + links).join("  |  ")]
}

// ----- summary -----
#if data.summary != "" [
  #section("Summary")
  #data.summary
]

// ----- experience -----
#section("Experience")
#for role in data.roles [
  #block(above: 9pt, below: 4pt)[
    #grid(
      columns: (1fr, auto),
      column-gutter: 8pt,
      [#text(weight: "bold")[#role.title], #role.company#if role.location != none [, #role.location]],
      text(style: "italic")[#fmt(role.start) -- #fmt(role.end)],
    )
  ]
  #for b in role.bullets [
    - #b.text
  ]
]

// ----- skills -----
#if data.skills_section.skills.len() > 0 [
  #section("Skills")
  #data.skills_section.skills.join(", ")
]

// ----- projects -----
#if data.projects.len() > 0 [
  #section("Projects")
  #for p in data.projects [
    #block(above: 7pt, below: 2pt)[
      #text(weight: "bold")[#p.name]#if p.url != none [ (#p.url)]
    ]
    #p.summary
  ]
]

// ----- education -----
#if data.education.len() > 0 [
  #section("Education")
  #for e in data.education [
    #grid(
      columns: (1fr, auto),
      column-gutter: 8pt,
      [#text(weight: "bold")[#e.institution]#if e.credential != "" [, #e.credential]],
      if e.end != none { text(style: "italic")[#fmt(e.end)] },
    )
  ]
]

// ----- certifications -----
#if data.certifications.len() > 0 [
  #section("Certifications")
  #for c in data.certifications [
    - #c.name#if c.issuer != "" [, #c.issuer]#if c.issued != none [ (#fmt(c.issued))]
  ]
]
