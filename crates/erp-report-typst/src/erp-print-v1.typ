// SPDX-License-Identifier: MIT OR Apache-2.0
// Version 1: input is a bounded, topologically ordered arena of typed data.
// Assemble children before parents: no recursive evaluator walk or parsed user markup.
#let d = sys.inputs
#set document(title: d.title)
#set text(font: d.fonts, lang: d.lang, size: 10pt)
// House table: full width shared by content (encode.rs column_weights), rules between rows, a shaded
// header (TableLook::Lined).
#let rule = 0.5pt + rgb("#ced4da")
#set table(stroke: (x: none, y: 0.5pt + rgb("#dee2e6")), inset: (x: 6pt, y: 5pt))
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
      if n.table == "bordered" { aligned = { set table(stroke: 0.5pt + rgb("#adb5bd")); aligned } }
      else if n.table == "borderless" { aligned = { set table(stroke: none); aligned } }
      if n.padding == 0 and not n.keep and not n.decorated { parbreak() + aligned + parbreak() }
      else {
        let pad = n.padding * 1pt
        let edge(on) = if on { calc.max(n.padding, 4) * 1pt } else { pad }
        block(inset: (x: pad, top: edge(n.rule_above), bottom: edge(n.rule_below)), breakable: not n.keep,
          width: if n.fill != none or n.rule_above or n.rule_below { 100% } else { auto },
          fill: if n.fill == none { none } else { rgb(..n.fill) },
          radius: if n.fill == none { 0pt } else { 3pt },
          stroke: (top: if n.rule_above { rule } else { none }, bottom: if n.rule_below { rule } else { none }),
          above: if n.above > 0 { n.above * 1pt } else { auto },
          below: if n.below > 0 { n.below * 1pt } else { auto },
          aligned)
      }
    } else { body }
  }
  else if n.kind == "grid" { grid(columns: n.columns.map(x => x * 1fr), gutter: n.gutter * 1pt, ..children) }
  else if n.kind == "table" {
    // A table without header rows has no header: an empty one would print an empty first row.
    let hrows = calc.ceil(n.header.len() / n.columns)
    let head = if hrows > 0 { (table.header(repeat: true, ..n.header.map(i => built.at(i))),) } else { () }
    table(columns: n.weights.map(w => w * 1fr), fill: (x, y) => if y < hrows { rgb("#f1f3f5") }, ..head, ..children)
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
