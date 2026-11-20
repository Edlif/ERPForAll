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
// A table row is never split across two pages: a row that does not fit moves to the next one. A row (or a
// kept block) that encode.rs estimates taller than half the page body may break across pages
// (`breakable`, `tall`), so nothing runs past the bottom margin.
#set table.cell(breakable: false)
#let built = ()
#for n in d.nodes {
  let children = n.at("children", default: ()).map(i => built.at(i))
  let content = if n.kind == "text" { if n.pre { raw(n.text, block: false) } else if n.size == none { text(n.text) } else { text(size: n.size * 1pt, n.text) } }
  else if n.kind == "flow" { children.join() }
  else if n.kind == "block" or n.kind == "inline" {
    let body = children.join()
    if n.pre { body = {
      show raw: set text(font: if n.font == "" { d.fonts } else { n.font }, size: 1em / 0.8)
      body
    } }
    if n.font != "" { body = text(font: n.font, body) }
    if n.rtl != none { body = text(dir: if n.rtl { rtl } else { ltr }, body) }
    if n.tracking != 0 { body = text(tracking: n.tracking * 1pt, body) }
    if n.underline { body = underline(body) }
    if n.strike { body = strike(body) }
    if n.baseline == -1 { body = sub(body) }
    else if n.baseline == 1 { body = super(body) }
    if n.link != "" { body = link(n.link, body) }
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
        block(inset: (x: pad, top: edge(n.rule_above), bottom: edge(n.rule_below)), breakable: not n.keep or n.tall,
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
  // A row's cells sit at its top unless the row asks for its middle or its bottom (align-items-*). The top is
  // written out: a page header is placed at the bottom of its margin, and a grid left to inherit that put its
  // cells at the bottom of the row, unlike the body and the preview.
  else if n.kind == "grid" { grid(columns: n.columns.map(x => x * 1fr), gutter: n.gutter * 1pt, align: if n.align == "center" { horizon } else if n.align == "end" { bottom } else { top }, ..children) }
  else if n.kind == "table" {
    // A table without header rows has no header: an empty one would print an empty first row.
    let hrows = n.header_rows
    let head = if hrows > 0 { (table.header(repeat: true, ..n.header.map(i => built.at(i))),) } else { () }
    // A plain child of a row that may break is made a breakable cell here; a cell node says so itself.
    let body = children.enumerate().map(((i, c)) => if n.loose.at(i) { table.cell(breakable: true, c) } else { c })
    table(columns: n.weights.map(w => if w == 0 { auto } else { w * 1fr }), fill: (x, y) => if y < hrows { rgb("#f1f3f5") }, ..head, ..body)
  }
  else if n.kind == "list" { if n.ordered { enum(..children) } else { list(..children) } }
  // A picture has a box (`width`, `height`) and the size it is drawn at inside it (`fitted`), both from encode.rs:
  // the whole picture in the middle of its box, or the box filled and what falls outside cut off (`clip`). It is
  // never stretched: `fitted` has the picture's proportions, and the pixels are drawn into exactly that size. It
  // never runs past what holds it: the box shrinks, both sides alike, into the width of its cell, column or page
  // body and the height of the page body.
  else if n.kind == "image" { layout(size => {
    let k = calc.min(1, size.width / (n.width * 1pt), size.height / (n.height * 1pt))
    let picture = image(n.data, format: n.format, width: n.fitted.at(0) * k * 1pt, height: n.fitted.at(1) * k * 1pt, fit: "stretch", alt: n.alt)
    if n.exact { picture } else { block(width: n.width * k * 1pt, height: n.height * k * 1pt, clip: n.clip, place(center + horizon, picture)) }
  }) }
  // A generated code: sized by encode.rs with one scale both ways (`fit: "contain"` says so again). The text a
  // person reads sits centred under it in figures of equal width, and the two never part: a box does not break.
  else if n.kind == "vector" {
    let code = image(n.data, format: "svg", fit: "contain", width: n.width * 1pt, height: n.height * 1pt, alt: n.alt)
    if n.caption == none { code }
    else { box(width: n.width * 1pt, align(center, stack(dir: ttb, spacing: 2pt, code, text(size: n.caption_size * 1pt, number-width: "tabular", n.caption)))) }
  }
  else if n.kind == "placeholder" { box(width: n.width * 1pt, height: n.height * 1pt, stroke: 0.5pt + gray, inset: 2pt, clip: true, text(n.alt)) }
  else if n.kind == "cell" { table.cell(colspan: n.colspan, rowspan: n.rowspan, breakable: n.breakable, children.first()) }
  else if n.kind == "rule" { line(length: 100%) }
  else if n.kind == "linebreak" { linebreak() }
  else if n.kind == "pagebreak" { pagebreak() }
  else if n.kind == "current" { context counter(page).display() }
  else if n.kind == "total" { context str(counter(page).final().first()) }
  else { panic("unsupported admitted node") }
  built.push(content)
}
#set page(width: d.width * 1pt, height: d.height * 1pt,
  margin: (top: d.margins.at(0) * 1pt, right: d.margins.at(1) * 1pt, bottom: d.margins.at(2) * 1pt, left: d.margins.at(3) * 1pt),
  header: if d.header == none { none } else { built.at(d.header) },
  footer: if d.footer == none { none } else { built.at(d.footer) })
#built.at(d.body)
