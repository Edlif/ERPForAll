// SPDX-License-Identifier: MIT OR Apache-2.0
// Version 1: input is a bounded, topologically ordered arena of typed data.
// Assemble children before parents: no recursive evaluator walk or parsed user markup.
#let d = sys.inputs
#set document(title: d.title)
#set text(font: d.fonts, lang: d.lang, size: 10pt)
#let built = ()
#for n in d.nodes {
  let children = n.at("children", default: ()).map(i => built.at(i))
  let content = if n.kind == "text" { text(n.text) }
  else if n.kind == "flow" { children.join() }
  else if n.kind == "block" or n.kind == "inline" {
    let body = children.join()
    if n.underline { body = underline(body) }
    body = text(size: n.size * 1pt, weight: if n.bold { "bold" } else { "regular" }, style: if n.italic { "italic" } else { "normal" }, fill: rgb(..n.color), body)
    if n.kind == "block" {
      let side = if n.align == "center" { center } else if n.align == "end" { end } else if n.align == "start" { start } else { panic("unsupported admitted alignment") }
      let aligned = { set align(side); body }
      if n.padding == 0 and not n.keep { parbreak() + aligned + parbreak() }
      else { block(inset: n.padding * 1pt, breakable: not n.keep, aligned) }
    } else { body }
  }
  else if n.kind == "grid" { grid(columns: n.columns.map(x => x * 1fr), gutter: n.gutter * 1pt, ..children) }
  else if n.kind == "table" {
    table(columns: n.columns, table.header(repeat: true, ..n.header.map(i => built.at(i))), ..children)
  }
  else if n.kind == "list" { if n.ordered { enum(..children) } else { list(..children) } }
  else if n.kind == "image" { image(n.data, format: n.format, width: if n.width == auto { auto } else { n.width * 1pt }, height: if n.height == auto { auto } else { n.height * 1pt }, alt: n.alt) }
  else if n.kind == "vector" { image(n.data, format: "svg", width: if n.width == auto { auto } else { n.width * 1pt }, height: if n.height == auto { auto } else { n.height * 1pt }, alt: n.alt) }
  else if n.kind == "placeholder" { box(width: n.width * 1pt, height: n.height * 1pt, stroke: 0.5pt + gray, inset: 2pt, clip: true, text(n.alt)) }
  else if n.kind == "cell" { table.cell(colspan: n.colspan, rowspan: n.rowspan, children.first()) }
  else if n.kind == "rule" { line(length: 100%) }
  else if n.kind == "linebreak" { linebreak() }
  else if n.kind == "pagebreak" { pagebreak() }
  else if n.kind == "current" { context counter(page).display() }
  else if n.kind == "total" { context str(counter(page).final().first()) }
  else { panic("unsupported admitted node") }
  built.push(content)
}
#set page(width: d.width * 1pt, height: d.height * 1pt, margin: d.margin * 1pt,
  header: if d.header == none { none } else { built.at(d.header) },
  footer: if d.footer == none { none } else { built.at(d.footer) })
#built.at(d.body)
