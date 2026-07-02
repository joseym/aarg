// Cover-letter template. Reads the CoverLetter payload that `aarg cover`
// and `tailor --cover` produce, and renders a one-page business letter.
// The greeting, body paragraphs, and sign-off are supplied by aarg; the
// contact block and recipient come from the payload; the date is today's.
//
// Strings from the payload are inserted with #-interpolation, so their
// contents (an email's "@", a stray "*") are literal text, never markup.

#let data = json(sys.inputs.data)
#let contact = data.contact

#set page(paper: "us-letter", margin: (x: 1in, top: 1in, bottom: 1in))
#set text(size: 11pt)
#set par(justify: false, leading: 0.68em, spacing: 1.05em)

// Header: name, then a single contact line (location, email, phone, links).
#text(size: 17pt, weight: "bold")[#contact.full_name]
#v(3pt)

#let head-parts = (contact.location, contact.email, contact.phone).filter(x => x != none and x != "")
#let link-parts = contact.links.map(l => l.url)
#let all-parts = head-parts + link-parts
#text(size: 9.5pt, fill: rgb("#555555"))[#all-parts.join("  ·  ")]
#v(6pt)
#line(length: 100%, stroke: 0.5pt + rgb("#dddddd"))
#v(14pt)

#datetime.today().display("[month repr:long] [day padding:none], [year]")
#v(2pt)

#data.greeting

#for para in data.paragraphs [
  #para

]
#v(10pt)
#data.signoff
