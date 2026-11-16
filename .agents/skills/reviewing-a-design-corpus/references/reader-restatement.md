# Why the context-free reader works this way

Background for "Have a reader with none of the author's context say it back" in
`../SKILL.md`. The rules are there; this is the reasoning and the examples.

## Why a reader at all

The author knows more than the page says, and so do reviewers who sat in the
same conversations. Both fill the gaps without noticing. The implementer who
arrives later with the document and nothing else cannot, and careful reading by
people who already know does not find what that reader will miss.

Before anyone argues with the design, one reader restates it. The pass runs
beside the numbers pass because neither needs anything from the other, and ahead
of every pass that argues.

## Why each rule on the reader

- **Fresh session, never a fork.** A fork restates the design correctly from
  memory, which is the failure under test.
- **Document and the files it links, nothing else.** Not the drafting
  conversation, not the author's notes, not a paragraph of orientation.
- **No agent instruction files.** They hold the team's vocabulary, so a reader
  who has them passes a document its real reader fails. Starting a session fresh
  keeps the conversation out and often lets these in, because many harnesses
  load them into every new session (reported against the upstream design this
  was adapted from: google/elephant-goldfish-model, issue #237). Hence the
  reader lists what it was given, and is never told to read them first.
- **Restates and does not review.** Its opinion of the design belongs to later
  passes, and asking for both thins the restatement.
- **One reader.** A second finds mostly the same gaps at twice the cost; this is
  a cheap first check, not a panel.

## What counts as a finding, with examples

- **Wrong:** the document says something other than was meant, or needs outside
  context to be read correctly.
- **Guessed or could not say:** a referent such as "the old importer" or "the
  agreed threshold" that the document relies on and does not contain.
- **Open list differs:** something the reader took as settled is still open for
  the author, or the reverse.
- **Echoed phrase:** "Because of the vendor's limit", repeated as the reason, is
  a guess in the document's words. Ask which vendor and what limit.

The reader is ignorant of the project, not of the trade: a general engineering
term it does not know is not a finding, and neither is its view of whether the
design is good.

## Fixing

The fix says what the author meant. Where the page was silent because nothing
was decided, the fix is to say it is open. Explaining the gap to the reader
fixes one reader, and that reader is then spent. Where nobody who holds the
author's intent is at hand, the guesses stand as findings on their own and the
restatement goes to the author to compare.
