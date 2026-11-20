---
name: office
description: Create, read and edit .docx, .xlsx and .pptx by running python-docx, openpyxl or python-pptx as an external program. Load before any document task.
---

# Office documents

This harness ships no document parser and no writer. Every `.docx`, `.xlsx` and
`.pptx` you produce is written by an external Python program that **you** run
with the shell tool. There is no fallback: a hand-written OOXML zip is a corrupt
file, and an empty document that opens is worse than an honest refusal.

## 1. Check the toolchain before writing anything

Run the shell tool with:

    forge doctor

Read the `office` line. It reports one of `missing`, `unknown (...)`, or the
interpreter that can import the library, for each of `docx`, `xlsx`, `pptx`,
`convert`:

    OK    office  docx=python 3.12 xlsx=python 3.12 pptx=missing convert=missing

If the format you were asked for is **not** reported as available — `missing`,
`unknown`, or absent from the line entirely — then:

* **STOP.** Do not create the file. Do not create an empty or stub file "to be
  filled later". Do not write XML or a zip by hand.
* Report which format is unavailable, that the Python library for it is not
  installed, and the command that installs it:
  `python -m pip install python-docx` (`.docx`),
  `python -m pip install openpyxl` (`.xlsx`),
  `python -m pip install python-pptx` (`.pptx`).
* Say plainly what you did not do. A refused document costs one message; a
  silently empty one costs a workday.

## 2. Write a script, then run it

Do not paste a multi-line program into a shell string: quoting differs between
`pwsh` and POSIX shells. Write a `.py` file with the file tool, run it, then
delete it if the user does not want it.

### Create a .docx

```python
from docx import Document

document = Document()
document.add_heading("Quarterly report", level=1)
document.add_paragraph("First line of the body.")
document.add_paragraph("Second line.", style="List Bullet")
table = document.add_table(rows=2, cols=2)
table.cell(0, 0).text = "region"
table.cell(0, 1).text = "total"
document.save("out.docx")
```

### Create a .xlsx

```python
from openpyxl import Workbook

book = Workbook()
sheet = book.active
sheet.title = "Summary"
sheet.append(["region", "total"])
sheet.append(["north", 1250])
sheet["D1"] = "=SUM(B2:B3)"          # formulas are text, never pre-computed
book.save("out.xlsx")
```

### Create a .pptx

```python
from pptx import Presentation

deck = Presentation()                # a blank deck has no slides: add them
slide = deck.slides.add_slide(deck.slide_layouts[1])
slide.shapes.title.text = "Status"
slide.placeholders[1].text = "One bullet\nAnother bullet"
deck.save("out.pptx")
```

### Read a document

```python
from docx import Document

for paragraph in Document("in.docx").paragraphs:
    if paragraph.text.strip():
        print(paragraph.text)
```

For spreadsheets iterate `openpyxl.load_workbook("in.xlsx").active.iter_rows(values_only=True)`;
for decks iterate `pptx.Presentation("in.pptx").slides`. Ask for a conversion
instead when the answer needs layout fidelity rather than values.

### Convert (PDF, ODT, legacy formats)

Only when `forge doctor` reports `convert` as available:

    soffice --headless --convert-to pdf --outdir . in.docx

LibreOffice is a converter, not a generator: use it to change a format, never to
create the content.

## 3. Verify before you report success

Every script ends by re-opening its own output and printing a count:

```python
from docx import Document
print("paragraphs:", len([p for p in Document("out.docx").paragraphs if p.text.strip()]))
```

If the count is zero, the file is not the deliverable — say so instead of
reporting it as done. State the path you wrote and the counts you measured.

## Rules

* One script per document, written to the workspace, run once. No GUI
  automation, and never leave LibreOffice running without `--headless`.
* Never claim a document was created, converted or verified unless the command
  that did it ran, and never report a count you did not print.
* The `python` in these snippets is whichever interpreter `forge doctor` named.
  If only `py -3` exists, use `py -3`; if only `python3`, use `python3`.