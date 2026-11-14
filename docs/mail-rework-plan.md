# Mail rework plan

Goal: make reading and answering mail pleasant. HTML mail should look right,
and replying should have a real compose window instead of a bare text box.

## Decisions

| Question | Choice |
|---|---|
| How HTML mail is shown | A native renderer inside Mail, with an "Open in browser" fallback |
| Where messages are written | A separate compose window |
| Images from the internet | Blocked until you click "Load images" |
| The editor | Plain text with simple formatting, converted to HTML on send |

## Where it stands

- Folders show raw server names (`INBOX.Drafts`).
- The selected message turns all its text blue, rows show no preview text, and
  dates read `2026-11-10 14:48`.
- A reply replaces the reading pane with a plain text box.
- HTML mail is flattened to text; links and images are lost.
- Every click reconnects to the server, which makes it feel slow.

## Phase 1: HTML mail

- Clean the HTML with a sanitizer (`ammonia`): no scripts, no trackers, no
  styles that load things from the internet.
- Render it natively: paragraphs, headings, bold/italic/underline, links, lists,
  quote bars, simple tables, rules and images. Layout tables in newsletters are
  flattened into one readable column.
- Links open in the browser, and the real address shows on hover, so a link that
  hides its destination is visible.
- Images embedded in the message (`cid:`) show at once. Remote images stay
  blocked behind a "Load images" button.
- "Open in browser" writes the cleaned HTML to a temporary file for messages that
  are too complex for the native view.
- Plain-text mail gets clickable links.

## Phase 2: Replying and writing

- A compose window for reply, reply all, forward and new messages.
- Fields: From, To, Cc, Bcc (hidden until needed), Subject. Address suggestions
  from people already mailed.
- Attachments as chips with a name and size, drag and drop, and a file picker
  instead of typing a path.
- Body is plain text; `**bold**`, links and lists become HTML on send, and the
  message is sent as plain text plus HTML.
- Replies put the new text above a collapsed quote of the original. Reply all
  picks the right recipients and leaves out your own address. Reply headers
  (`In-Reply-To`, `References`) keep the message in its thread.
- Drafts save to the Drafts folder, and sent mail is copied to Sent when the
  server does not do it itself.

## Phase 3: Look and feel

- Friendly folder names, order and icons, with unread counts on every folder.
- Message rows: sender, relative date (Today 14:48, Yesterday, Mon), subject, a
  two-line preview, an unread dot and a paperclip. Selection is a subtle tint,
  not all-blue text.
- Reading header: initials avatar, sender name and address, expandable To/Cc,
  and a toolbar for reply, reply all, forward, archive, trash, mark unread.
- Keyboard shortcuts: arrow keys, R to reply, Delete.

## Phase 4: Speed and reliability

- Keep one server connection open instead of reconnecting on every click. This
  is the main speed fix and comes first.
- A local cache, so folders and opened messages show instantly and offline.
- New-mail alerts through IMAP IDLE, with a desktop notification.
- "Load older messages" instead of the fixed 60.

## Later

Conversation threading, several accounts, and Gmail/Outlook browser sign-in
(needs an OAuth client ID registered by the account owner).

## How it will be checked

A set of sample emails in the repository (a newsletter, an Outlook-style table
layout, plain text, inline images, a non-English message) with automated tests
for the sanitizer and the renderer, run with `cargo test`. Then real use against
a real mailbox.

## Risks

- A native renderer will not match a browser pixel for pixel. Heavy marketing
  layouts will look simplified, which is what the browser button is for.
- HTML mail that displays correctly in Gmail and Outlook is fiddly to write; the
  generated HTML is kept simple on purpose.
