# emeraldian reading view: what it can gain

Research card t_2617e536. Read-only: no repo file changed, nothing installed, `~/Vault` and `~/.config` untouched.

## How this was checked

- **Binary:** the installed fork binary `~/.local/bin/emeraldian`, sha256 `22df27b5…`, the same file as `target/release/emeraldian` of `nvim-embed` at ec65d1c (the last commit is docs-only).
- **Code:** read at ec65d1c. The reading renderer is `crates/emeraldian/src/ui/note.rs`, the parser `crates/emeraldian-core/src/markdown.rs`, the theme `crates/emeraldian-theme`.
- **Live run:**
  - **Terminal:** real Kitty 0.49.2 with Malte's `~/.config/kitty/kitty.conf` (CaskaydiaMono Nerd Font, Omarchy theme include). It ran off-screen on an `Xvfb :77` display with the X11 backend, so nothing appeared on the desktop.
  - **Driving and screenshots:** Kitty remote control (`send-key`, `get-text`) drove the app, and ImageMagick `import` took the screenshots.
  - **Environment:** a run script with `OBSIDIAN_CLI=off`, a private `XDG_CONFIG_HOME`/`DATA`/`STATE`/`CACHE`, and a byte copy of Malte's `config.toml`. The only change in that copy was `reading_mode = true`.
  - **Vault:** a scratch vault with `Showcase.md` (every Obsidian construct), `Prose.md` (ordinary long prose), `Other.md`, `Flow.mmd`, `Sketch.excalidraw` and two PNGs.
- **Themes:**
  - Malte's live Omarchy theme (Aether-generated, black background).
  - Three stock Omarchy themes: tokyo-night, catppuccin-latte (light) and gruvbox. For each I rendered `/usr/share/omarchy/default/themed/kitty.conf.tpl` from the theme's `colors.toml`, exactly as Omarchy does, and layered it over his `kitty.conf`.
- **Kitty features:** a separate Kitty demo shot tested OSC 66 text sizing, styled underlines, OSC 8 links and Nerd Font glyphs (shot 12).
- **External tools:** a cited research pass (sources at the end) covered Obsidian, render-markdown.nvim, markview.nvim, glow/glamour, mdcat, Frogmouth, snacks.image and the Kitty protocol docs.
- **Caveat:** Xvfb runs at 96 dpi with `font_size 13`, while Malte runs Wayland HiDPI at size 9. Colours, glyphs and layout are the same; pixel sharpness and absolute sizes are not.

Screenshots, attached to the card:

| File | Shows |
|---|---|
| `01-showcase-top-malte-theme.png` | frontmatter, headings H1–H6, inline styles, callouts (Malte's theme) |
| `02-showcase-p2.png` | lists, tasks, code blocks, table head |
| `04-showcase-p4.png` | Mermaid, braille Excalidraw, embeds, image, raw HTML/footnote |
| `11-showcase-focus-mode-F8.png` | F8 focus mode (the only centred, narrower column today) |
| `theme-malte-2-prose-focus.png` | ordinary prose note, focus mode |
| `theme-catppuccin-latte-1-prose.png` | light theme, normal mode |
| `theme-gruvbox-3-showcase-mid.png`, `theme-tokyo-night-3-showcase-mid.png` | tasks, code, table, maths in two dark themes |
| `12-kitty-capabilities-demo.png` | what Malte's Kitty can draw: 2× and 1.5× text, superscript, curly/dashed/dotted coloured underlines, Nerd Font checkboxes vs the U+2611 fallback |

## 1. What exists today and what is missing or broken

| Feature | Status | Evidence (screen text from the live run, or code/test) |
|---|---|---|
| Callouts | ✅ bar + bold coloured title, nested callouts work | `▎ Expanded warning` / `▎ ▎ Nested example` |
| Foldable callouts `[!x]-` | ❌ always open, no fold glyph | `▎ Folded tip (collapsed by default)` followed by `▎ This body should be hidden until expanded.`; the parser only strips the marker (`foldable_callout_marker_is_not_part_of_the_title`) |
| Callout icons / full type table | ❌ no icons; `abstract`, `info`, `todo`, `example` all get the `note` colour | `Palette::callout()` in `emeraldian-theme/src/theme.rs` |
| Footnotes `[^1]`, `^[…]` | ❌ raw | `A footnote here[^1] and an inline one^[Inline footnote text.]`; definition line shown raw at the end: `[^1]: The footnote definition text.` |
| Tables | ✅ box drawing, column alignment, horizontal pan | `│ code      │ #tag  │    22 │` |
| Table cell formatting | ❌ bold, links, tags and code lose their style; cells truncated at 40 columns, no wrap | `│ Bold cell │ Other │` in plain body colour (`shown_text`, `MAX_COLUMN = 40` in `note.rs`) |
| Task lists | ✅ fixed in R3b: nine states, each with its own Nerd Font box and palette colour (`[x]`/`[-]` struck, `[-]` also dim) | was: `☑ Cancelled task`, `☑ Forwarded task` and `☑ Question task` all green and struck through. Now `reading::tests::every_task_state_has_its_own_glyph_colour_and_style`, live SGR in `docs/reading-view/shots/r3-showcase-*.png` |
| Task glyphs in Malte's font | ✅ fixed in R3b: `nf-md` code points U+F0131, F0132, F06F2, F0736, F0733, F078B, F0027, F0856, F0134, each checked with `fc-list ':charset=<hex>'` (all 15 CaskaydiaMono Nerd Font files) | was: `☐`/`☑` (U+2610/2611) not in CaskaydiaMono Nerd Font; Kitty fell back and the done box drew as a green blob (shots 02, 12) |
| Heading folding | ❌ none | `handle_reading` in `keys.rs` has no fold key; no fold state in `note.rs` |
| TOC / outline jump | ✅ outline panel; Enter jumps | moving 9 rows down + Enter landed on `Table` with `┌───────────┬…` at the top |
| Frontmatter / properties | ⚠️ plain `key: value` lines + a 60-column rule; lists joined; no types, no hide option | `tags: projekt/emeraldian, lesen` |
| Tags | ✅ inline chips | dark themes fine. On catppuccin-latte the chip is magenta `#ea76cb` on muted `#acb0be` = **1.22:1** contrast (computed) |
| Note embeds `![[note]]`, `![[note#heading]]`, `![[note#^id]]` | ❌ shown as a link only | `Other › Section` / `Other › ^blk7` / `Other`; deliberate, test `an_obsidian_embed_of_an_image_is_a_picture_but_of_a_note_is_not` |
| Image / Excalidraw / Mermaid embeds | ✅ | the image is drawn with Kitty unicode placeholders (screen text is U+10EEEE cells), the braille drawing and the Mermaid boxes as cells |
| Block references | ⚠️ the link renders and the trailing `^id` is hidden since R3b; following the link still does not scroll | was: `A block with an id. ^para1` visible |
| Heading/block anchors when following a link in reading mode | ❌ the target opens at the top | `[[Other#Later]]` + Enter shows `Other` / `Intro paragraph…` at the top. `App::jump_to_anchor` returns unless nvim is editing; `follow_first_link` builds `LinkTarget::plain` |
| Maths | ✅ inline and display, centred | `Inline E = mc² …`, `∫₀¹ x² dx = ─` with a stacked fraction |
| Code highlighting | ⚠️ keyword highlighter for rust/python/js/ts/sh/go only; toml, yaml, lua and the rest are plain | `toml` block plain in all themes (shots) |
| Code block panel | ❌ under Malte's `theme = "terminal"` | `code_bg = bg_alt = reset`, so code sits on the page background; vision and shots agree |
| Copy a code block | ❌ no key; mouse capture is on (`main.rs:408`) | `handle_reading` has no copy action |
| Link hover / preview popup | ❌ none | `handle_mouse` handles Down/Drag/Up/Scroll only; no popup code |
| Backlink peek | ⚠️ backlinks panel with raw, ~30-column snippets, one row per occurrence | `Showcase` / `…==highlight==, \`inline cod…` repeated 4× |
| Reading width / centering | ⚠️ only F8 focus mode: hard-coded `FOCUS_WIDTH = 100` (`ui/mod.rs:226`), and it hides the panes | at 176 columns, normal prose runs about 160 characters per line |
| Zoom | ❌ in the app (Kitty's own font-size keys only) | — |
| Search in note | ⚠️ Ctrl+F picker lists matching *source* lines and Enter scrolls there; no match highlight, no n/N | `› #### Fourth level   line 21`; after Enter the match is coloured as a heading only |
| Scroll memory per note | ⚠️ kept while the tab stays open; lost on close/reopen and on restart | Showcase scrolled to `Callouts`, then follow + Alt+Left kept `Callouts`; F3 + Ctrl+Shift+T reopened at `title: Showcase`; `state.rs` stores only expanded folders |
| Extra leaks found | ✅ fixed in R3b (`%%comments%%`, `***both***`, hard breaks, `<kbd>`/`<br>`; `<mark>`, `<sub>`, `<sup>` too) | was: `%%secret comment%%` visible; `***both***` shown as `*both*`; a two-space hard break lost (`push_str(candidate.trim())`); `<kbd>`/`<br>` shown raw. Now `markdown.rs` tests and `the_showcase_note_reads_without_leaks_and_is_never_changed` |

Not settled: `kitten send-key ctrl+bracketright` did not toggle the outline in my run, while the palette entry "Toggle outline sidebar Ctrl+]" did. I did not check whether Malte's real Ctrl+] works.

## 2. Facts about Malte's setup that shape the proposals

- **Theme mapping.** Malte runs `theme = "terminal"`. That seed uses ANSI names (`seed.rs` `TERMINAL`), and Omarchy's `kitty.conf.tpl` fills those slots with `color0 = background`, `color7 = foreground` and `color8 = muted`. Consequences:
  - **Muted text is not muted.** `text_muted = "gray"` is ANSI 7, which is `foreground`. Quote text, frontmatter values, H6 and syntax punctuation therefore show at full body colour. The vision review of the shots flagged the quote as "identical to body".
  - **No second surface.** `bg_alt`/`code_bg` are `reset`, so there is no code panel and inline code has no background.
  - **The Omarchy accent is ignored.** The accent is ANSI magenta, not Omarchy's `accent`. On catppuccin-latte, magenta on the background is 2.34:1, while the theme's own accent `#1e66f5` would be 4.34:1. Magenta carries 7 roles there: H1, links, tags, bullets, quote bar, frontmatter keys and key hints.
  - **Live recolouring.** Indexed colours are resolved by Kitty, and `omarchy-restart-terminal` reloads Kitty with SIGUSR1. An Omarchy theme switch should therefore recolour emeraldian live. This is how indexed colours work in a terminal; I did not test it live.
- **Omarchy colour roles.** Omarchy v4 themes ship `colors.toml` with 26 roles:
  - **Surfaces:** `background`, `dark_background`, `darker_background`, `lighter_background`.
  - **Text:** `foreground`, `dark_foreground`, `light_foreground`, `bright_foreground`, `muted`.
  - **Selection and accent:** `selection`, `accent`.
  - **Hues:** red, orange, yellow, green, cyan, blue, magenta, brown, plus six `bright_*`.
  - **Coverage:** all 23 stock themes have them; `orange` and `brown` are missing in 3.
  - **Templates:** Omarchy renders templates from `~/.config/omarchy/themed/*.tpl` and `/usr/share/omarchy/default/themed/*.tpl` into `~/.local/state/omarchy/current/theme/` on every switch (`omarchy-theme-set-templates`).
  - **Omarchy's own Obsidian mapping** (`obsidian.css.tpl`): H1 red, H2 green, H3 yellow, H4 blue, H5/H6 magenta, links blue, accent = `accent`.
- **User themes in emeraldian.** emeraldian already loads user themes from `~/.config/emeraldian/themes/*.toml`, with `extends`, through `presets::load_custom`. It reads them only once, in `App::new`, so there is no reload.
- **What Malte's Kitty can draw** (verified in shot 12, Kitty 0.49.2):
  - OSC 66 at 2× (`s=2`) and 1.5× (`s=2:n=3:d=4`).
  - A raised small digit (`n=1:d=2:w=1`).
  - Curly, dashed and dotted underlines with their own colour (SGR `4:3/4:4/4:5` + `58`).
  - Dim, italic, bold-italic and strikethrough.
  - Nerd Font checkbox glyphs (U+F0131/F0132).

  The protocol docs confirm the cell maths: an `s=2` heading takes 2 rows, and overwriting part of it erases the whole glyph.[1]
- **Images already use unicode placeholders.** `ratatui-image` writes Kitty unicode placeholders, so images scroll as text cells.[2][12] It also shows how to put an escape into one cell and mark the cells it covers as skipped (`set_skip`, kitty protocol module). That is the hook OSC 66 headings or OSC 8 links would need.
- **Input.** emeraldian enables the Kitty keyboard protocol (`DISAMBIGUATE_ESCAPE_CODES`, `main.rs:423`) and mouse capture.

## 3. Honest aesthetic verdict of the current reading view

- **Good:**
  - **Calm overall.** The palette stays cohesive in every dark Omarchy theme, and the body text has strong contrast.
  - **Headings and inline styles.** Headings read clearly by hue, with an underline rule on H1/H2. Links are underlined accent, unresolved links are dim, external links are blue, and highlights read as a strong amber block.
  - **Blocks.** Tidy box tables with right-aligned numbers. The Mermaid boxes look crisp. Display maths is centred with a stacked fraction.
  - **Focus mode.** F8 is the nicest state today (shot `theme-malte-2-prose-focus`).
- **Off, in order of how much it hurts daily reading:**
  1. **Measure.** About 160 characters per line in the normal view. Even F8's 100-column pane leaves about 85 characters of prose, above the 60–80 range glamour and Obsidian aim for.[16][31] Margins are lopsided: ribbon, 2 padding columns and a blank 4-column line-number gutter on the left, almost nothing on the right.
  2. **Raw syntax leaking into the page.** `%%comment%%`, `^para1`, `[^1]`, `^[…]`, `*both*`, `<kbd>`, `<br>`. These make a finished note look unrendered.
  3. **Theme mapping.** No code panel, muted text that equals body text, a quote that looks like body text. On the light theme the magenta overload hurts: H1 is the least legible heading, and the tag chip is 1.22:1.
  4. **Tasks.** All non-empty states share the same struck-through green fallback blob, and "Question" is struck out, which is semantically wrong.
  5. **Rhythm.** Spacing comes only from the author's blank lines (`BlockKind::Blank`), so lists, quotes and headings glue together or float. Rule lengths are arbitrary: the frontmatter rule is 60 columns, the heading rules are "text + 8". Every bullet level uses the same `•`.
  6. **Embeds.** Note embeds are bare links. Images, drawings and diagrams sit left-aligned with no caption and no frame, and dark screenshots melt into a black background.
- **Per theme:**
  - **Malte's black theme:** looks best.
  - **tokyo-night and gruvbox:** pleasant, with the same structural issues.
  - **catppuccin-latte:** the weakest, about "6/10 for reading comfort" in the vision review, for the contrast reasons above.

## 4. Ranked proposals

Ranked by daily-reading value divided by effort. Effort: **S** about one coder card / one day, **M** a few days, **L** a week or more, or real risk.

**Hotspot:** almost every item touches `crates/emeraldian/src/ui/note.rs` (3,380 lines: renderer, tables, highlighting and editor painting in one file). Before items 5–11 land, split the reading renderer out, for example into `ui/reading/{blocks,inline,table,code}.rs`, or run those cards strictly in sequence.

### 1. Readable, centred text column (S)

- **What you see:**
  - Prose wraps at a configurable width, for example `[ui] reading_width = 80`, centred in the note pane with equal margins, in normal reading mode, not only in F8.
  - The blank line-number gutter is not reserved in reading mode.
  - F8 keeps hiding the panes but uses the same width, replacing the hard-coded 100.
- **Why it fits:** a terminal cannot change line height or font size, so the measure is the strongest typographic lever it has. Malte's 3200 px screen makes very long lines the normal case.
- **Touches:**
  - `ui/note.rs`: `draw`, `draw_reading`, `prose_area`; `Picture.indent` for images and drawings.
  - `ui/mod.rs`: `readable`, `FOCUS_WIDTH`.
  - `config.rs`: `UiConfig`.
- **Risks:**
  - **Wide content:** tables, Mermaid and display maths. Either pan inside the column, as today, or let them break out to the pane width (decide).
  - **Find/outline rows:** they still map through `anchors`, so they should keep working; test them.
  - **nvim editing:** the edit pane already uses the full body (`draw_editing` → `draw_nvim` gets the whole area), so Ctrl+E already reflows today. Whether nvim should follow the column, like a zen mode, is a separate choice.
- **Copy from:**
  - Obsidian's "Readable line length" (default about 700 px) and Minimal's `--line-width: 40rem`.[31][32][33]
  - glamour's 80-column wrap with a 2-cell margin.[15][16]
  - hoonkim/mdcat's symmetric margins.[17]

### 2. Obsidian syntax hygiene (S)

- **What you see:**
  - **Hidden:** `%%inline%%` and `%%` block comments, and trailing block ids `^id`.
  - **Line breaks:** two trailing spaces and `<br>` break the line.
  - **Emphasis:** `***x***` renders bold-italic.
  - **`<kbd>`:** rendered as a key chip (reverse/`selection` background).
  - **Task states, each with its own Nerd Font glyph and colour:** `[-]` cancelled (faint, struck), `[>]` forwarded, `[?]` question, `[/]` in progress, `[!]` important. Only `x`/`X`/`-` are struck.
  - **Glyphs:** `☐`/`☑` are replaced by glyphs the font actually has.
- **Why it fits:** it removes the most visible "unrendered" signals at little cost, and the glyph fix is specific to Malte's font.
- **Touches:**
  - `emeraldian-core/src/markdown.rs`: `parse_blocks` (block comments, hard breaks), `parse_inline`/`match_inline` (`%%`, `***`, `<br>`, `<kbd>`), `list_item`.
  - `Marker::Task(bool)` → `Task(char)`. It is used only in `markdown.rs` and `note.rs`, so the change stays local.
  - `ui/note.rs` (`render_block`, `style_spans`), `ui/mod.rs` `icons`.
- **Risks:**
  - `%%` inside code spans and fences must stay visible.
  - An unclosed block comment hides the rest of the note in Obsidian; copy that behaviour on purpose.[24]
  - Obsidian treats any character in the brackets as complete,[24] so task counting must not change.
- **Copy from:**
  - Obsidian's comment rules and "any character marks it complete".[24]
  - render-markdown.nvim's checkbox `custom` state table (raw → icon + highlight).[13]
  - ITS theme's task set `[-] [>] [D] [?]`.[36]

### 3. An `omarchy` theme from `colors.toml`, hot-reloaded (S–M)

- **What you see:**
  - Every colour follows the active Omarchy theme by role, not by ANSI slot.
  - Code panels and inline code sit on `lighter_background`.
  - Muted text is really muted: a blend of `foreground` toward `background`, because Omarchy has no mid-grey text role.
  - Accent and links use `accent`; tag chips use `selection`.
  - Callout and heading hues come from the named hues.
  - Switching the Omarchy theme recolours a running emeraldian within about 1 s.
- **Why it fits:** this is exactly Malte's setup. It fixes the three theme defects in §2 at once, including the 1.22:1 tag chip on light themes. nvim-notes already hot-reloads through `omarchy-theme-hotreload`, so both panes stay in sync.
- **Touches:**
  - `emeraldian-theme`: a new loader, or a `Seed` built from `colors.toml`, in `presets.rs`/`seed.rs`.
  - `app.rs`: theme resolution in `App::new` and a cheap mtime poll in the main loop of `~/.local/state/omarchy/current/theme/colors.toml` (or `theme.generation`). No new dependency, no inotify crate. `ReadingCache` already re-lays out, because its key includes `Palette`.
  - `config.rs`: path override for tests.
- **Risks:**
  - The Omarchy v4 format could change; fall back to `terminal` when the file is missing or partial.
  - The `muted` derivation needs a contrast test on light and dark themes.
  - Dark screenshots on a black background still need item 5's frame.
- **Copy from:**
  - Omarchy's own `obsidian.css.tpl`, as the role → Obsidian-variable mapping (the H1–H6 hue assignment is a choice for Malte).
  - markview.nvim's colour-scheme-following highlight groups.[14]
- **Alternative:** a user template `~/.config/omarchy/themed/emeraldian.toml.tpl` rendered into the theme folder. That puts a chezmoi-managed file into the loop, and reading `colors.toml` directly needs nothing outside emeraldian.

### 4. Anchored navigation and remembered positions (S–M)

- **What you see:**
  - `[[Note#Heading]]` and `[[Note#^id]]` open the target scrolled to the heading or block in reading mode, as they already do in nvim.
  - Back/forward and reopening a tab (Ctrl+Shift+T) restore the scroll row.
  - A restart reopens each note where it was left.
- **Why it fits:** daily reading means hopping between notes. Losing the place is the most frequent small annoyance, and anchor links are pointless when they drop you at the top.
- **Touches:**
  - `app.rs`: `jump_to_anchor` gets a reading branch that uses `app.reveal` and `anchor_row`; `open_note`; `reopen_closed_tab`.
  - `keys.rs`: `follow_first_link` keeps the anchor; the link-selection card also uses this path.
  - `state.rs`: a per-vault `rel → scroll` map, capped.
  - `ui/note.rs`: anchors for a block id.
- **Risks:**
  - Coordinate with the running link-selection card, which shares the follow path.
  - Scroll rows depend on width; store the source line and resolve it via `anchors`.
- **Copy from:** Frogmouth's browser-style navigation stack and history.[19]

### 5. Typography and rhythm pass, including figure framing (S–M)

- **What you see:**
  - **Normalised spacing:** one blank row before every heading, runs of blanks collapsed, one blank row around lists, quotes, callouts, code and figures.
  - **Rules:** heading rules span the text column (H1 heavier `━`, H2 `─`), and the frontmatter rule matches.
  - **Heading levels:** told apart by weight and style as well as hue. H3 bold, H4 bold-dim, H5 italic, H6 dim small-caps-like uppercase.
  - **Bullets:** cycle per level (`•` `◦` `▪`).
  - **Quotes:** italic and muted.
  - **Figures** (images, Excalidraw, Mermaid, display maths):
    - centred in the column;
    - a faint caption line from alt text / alias / file name;
    - a thin frame or `lighter_background` mat for images, so dark screenshots don't melt into a black page;
    - the same spacing above and below.
- **Why it fits:** these are the levers a terminal really has (blank rows, cell margins, bold/dim/italic/underline, box glyphs). The vision review's "uneven rhythm" and "quote looks like body" complaints are covered here.
- **Touches:**
  - `ui/note.rs`: `render_document` gets a spacing post-pass over block kinds; `render_block` (Heading, ListItem, Quote, Rule); `lift`/`draw_pictures` for centring and captions.
  - `ui/mod.rs` icons; `emeraldian-theme/theme.rs` for H-level modifiers.
- **Risks:**
  - Spacing changes move every `anchors` row (outline, find), and tests on exact rows will need updating.
  - Centring pictures changes `Picture.indent`, which the pan logic uses.
- **Copy from:**
  - Minimal's "headings barely change size, smaller levels by weight and small-caps".[33]
  - render-markdown.nvim's per-level bullets and full/block heading backgrounds.[13]
  - glamour's `│ ` quote bar and `• ` bullets.[15]

### 6. Callouts: fold, icons, full type table (M)

- **What you see:**
  - `[!x]-` callouts show only their title with `▸`, and `[!x]+` show `▾`.
  - A key toggles the callout nearest the reading focus row; `zM`/`zR`-style keys fold or unfold all.
  - Each type gets a Nerd Font icon and Obsidian's aliases and colours: abstract cyan, info/todo blue, example purple, bug/failure red.
  - Optionally a "block" style with a `lighter_background` tint.
- **Why it fits:** callouts carry Malte's structure. Folding long ones turns a long note into a scannable page without a GUI.
- **Touches:**
  - `markdown.rs`: keep the fold flag in `BlockKind::Callout`.
  - `ui/note.rs`: `render_block` Callout.
  - `emeraldian-theme/theme.rs`: `callout()`, plus new roles if needed.
  - `keys.rs`: `handle_reading`.
  - `app.rs`: fold state per tab, and `ReadingCache` key.
- **Risks:**
  - Which callout a key acts on needs a focus row. Share the cursor concept with the link-selection card instead of inventing a second one.
  - Fold state must invalidate the cache.
- **Copy from:**
  - Obsidian's callout spec: types, aliases, `+`/`-`, nesting.[21]
  - render-markdown.nvim's callout table `raw → rendered icon + highlight + category`.[13]
  - ITS's callout style switch (original/block/outline) and its `[!type|attr]` syntax.[36][37]

### 7. Tables and properties that look designed (S–M)

- **What you see:**
  - **Table cells** keep inline styles: bold, code, links in link colour, tag chips. Long cells wrap inside the column instead of truncating at 40. The header separator is heavier (`━`), with optional zebra rows on `lighter_background`.
  - **Frontmatter** becomes a properties card:
    - aligned keys;
    - `tags` as chips and `aliases` dim;
    - dates, booleans and numbers styled by type;
    - a `[ui] properties = "visible" | "hidden" | "source"` switch, as in Obsidian.
- **Why it fits:** tables are already good structurally; inline styles and wrapping are what make them read like the rest of the note.
- **Touches:**
  - `ui/note.rs`: `render_table` (replace `shown_text` with `style_spans` + `wrap_spans` per cell), Frontmatter arm.
  - `markdown.rs` `parse` gets typed values instead of `as_scalar()`; `emeraldian-core/src/note.rs`; `config.rs`.
- **Risks:** row heights become variable, so the pan logic and `widest` must use real widths.
- **Copy from:**
  - render-markdown.nvim's padded cell mode and border presets.[13]
  - markview.nvim's typed property decorations.[14]
  - Obsidian's property display modes.[25]

### 8. Find in note, done properly (S–M)

- **What you see:**
  - `/` in reading mode (or Ctrl+F) types a query.
  - Every match in the rendered view gets the `selection` background, and the current match gets `accent`.
  - `n`/`N` step through matches, and a `3/7` counter shows in the status line.
  - Esc clears the highlight.
- **Why it fits:** `/ n N` is muscle memory for a vim user; today's picker lists source lines and loses the context.
- **Touches:**
  - `keys.rs`: `handle_reading` (`/`, `n`, `N`).
  - `ui/note.rs`: an overlay pass over the visible `Line`s in `draw_reading`, matching rendered text, not source.
  - `modal.rs`/`actions.rs`: `update_find`.
- **Risks:** matches that span wrapped rows; `row_showing` already handles the two-row case.
- **Copy from:** Obsidian's in-note find with Previous/Next,[29] and less/vim.

### 9. Note transclusion: `![[note]]`, `![[note#heading]]`, `![[note#^id]]` (M)

- **What you see:**
  - The embedded note, section or block is rendered inline. It gets a faint left bar or frame and a small caption, `↳ Other › Section`, that is also a link.
  - Nested embeds are capped by the existing depth/cycle guard.
- **Why it fits:** embeds are a core Obsidian reading feature,[23] and the guard (`MAX_EMBED_DEPTH`, `ctx.embeds`) is already in place.
- **Touches:**
  - `ui/note.rs`: `lift`/`embedded_diagram` gets a Markdown branch, plus `render_document` for the slice.
  - Section and block extraction belongs in `emeraldian-core`: heading ranges from the parser, block ids from item 2.
- **Risks:**
  - **Cache.** `ReadingKey` covers only the open note's mtime/len, so an edited embedded note would show stale content. The key needs the mtimes of every embedded file.
  - **Outline and find** must ignore embedded headings.
  - **Value is uncertain:** I could not check how often Malte's vault uses note embeds, because `~/Vault` was off limits.
- **Copy from:** Obsidian's embed semantics, live-updating[23] with `#heading`/`#^id` addressing,[22] and markview.nvim's distinct embed rendering.[14]

### 10. Footnotes (M)

- **What you see:**
  - `[^1]`/`[^name]` and inline `^[…]` become small raised numbers in order of first use. Kitty `OSC 66 n=1:d=2` superscript was verified in shot 12; Unicode `¹²³` is the cheap fallback.
  - The definitions move into a "Footnotes" block at the end under a rule, dim, with back-references.
- **Why it fits:** research and citation-heavy notes use them, and today they are the most visible raw syntax after comments.
- **Touches:**
  - `markdown.rs`: a new `SpanKind::FootnoteRef`, a `BlockKind::FootnoteDefs` collected in `parse`; keep the existing `[^1_2]` emphasis guard.
  - `ui/note.rs`.
- **Risks:**
  - Multi-line definitions (indented continuation).
  - Superscripts via OSC 66 need the skip-cell writer; start with Unicode superscripts.
- **Copy from:**
  - Obsidian: named footnotes still show as numbers; inline `^[…]`.[24]
  - markview.nvim's footnote and superscript rendering.[14]

### 11. Code blocks as panels, with more languages and copy (M)

- **What you see:**
  - **Panel:** a filled panel on `lighter_background` (this comes from item 3), the language as a small tab at the panel's top-right, and one padding column.
  - **Highlighting** extends to toml/yaml/json/lua/sql/rust/nix with the existing hand-written highlighter. A real grammar (syntect) only if Malte approves a new dependency.
  - **Copy:** `yy` copies the code block nearest the reading focus row via the existing `crate::clipboard`, with OSC 52 as an option.[9]
- **Why it fits:** Malte's notes are a developer's notes. The panel is the single biggest visual fix for code, and copying without dragging across the gutter and ribbon is daily use.
- **Touches:**
  - `ui/note.rs`: Code arm, `highlight`, `keywords_for`, `comment_prefix`.
  - `keys.rs`; `clipboard.rs`.
- **Risks:**
  - The copy target needs the same focus-row concept as items 6 and 4; share it with the link-selection card.
  - syntect is a heavy dependency, so ask first.
- **Copy from:**
  - render-markdown.nvim's code block border, language icon/name position and padding.[13]
  - mdcat's syntect use.[18]

### 12. Headings in real sizes with the Kitty text sizing protocol (L)

- **What you see:** H1 at 2× (or 1.5× via `s=2:n=3:d=4`, verified in shot 12), H2 at 1.5×, H3+ by weight/hue as in item 5. A heading line then takes 2 terminal rows.
- **Why it fits:** this is the one thing only Kitty can do here, and it moves the reading view closest to Obsidian's look.[1]
- **Touches:**
  - A custom cell writer in `ui/note.rs` that puts the OSC 66 escape into the first cell and marks the covered cells skipped, as ratatui-image does for its Kitty placeholders.[12]
  - `ReadingCache` row model; `anchors`; find; scroll clamps.
- **Risks (real):**
  - A heading partly scrolled off the top cannot be drawn half, because the protocol erases the whole multicell glyph when any part is overwritten. It needs a fallback to plain bold for that frame.[1]
  - A heading wider than the column must wrap at double width.
  - Mermaid/drawing code that writes nearby cells must never touch the block.
  - A capability probe (CPR) at start-up.[1]
  - hoonkim/mdcat reports fractional scales are not honoured on every Kitty build and uses integer scales only.[17] On Malte's 0.49.2 they work.
- **Copy from:** hoonkim/mdcat (Rust, Kitty-only, H1 3×, H2 2×, synchronized output).[17]

## 5. Considered, not ranked into the first twelve

- **Link and backlink peek popups.** A rendered preview of the selected link's target, Obsidian Page preview style.[27] Backlinks rendered through the reading renderer, grouped per note, optionally as a "Linked mentions" footer. Value is medium; it waits for the link-selection card, which provides "the selected link".
- **Kitty-native link layer:**
  - OSC 8 for external URLs, opened with Kitty's `ctrl+shift+p>y` hints even under mouse capture.[4][5][7]
  - Curly red underline for unresolved links, dashed for block refs and dotted for tags.[3]
  - Both need the same skip-cell escape writer as item 12. Do not emit wikilinks as OSC 8: `xdg-open` on an `obsidian://` URI would start the Obsidian GUI.
- **Heading folding** (Obsidian fold commands).[28] Lower value once item 6 exists, plus the outline jump; the same fold machinery could be reused later.
- **Highlight colours** `==🔴…==`[24] and **in-app zoom.** Kitty's own font-size keys already zoom.
- **Pixel-rendered maths and diagrams, snacks.image style.** That pipeline renders LaTeX/Typst/Mermaid to images through ImageMagick and shows them through Kitty placeholders.[38] Rejected for diagrams, because Malte chose the braille/text look. Not needed for maths, which is already typeset in Unicode and reads well in the shots.
- **Gutter heading markers** (`H2` beside each heading, a forum CSS snippet).[35] This is a cheap optional toggle inside item 5, since the gutter column is free in reading mode.

## 6. Recommended first batch: three coder cards

The three touch different layers, apart from `ui/note.rs` (items 1 and 2), so run card A before card B or have B rebase on A. Card C is independent (`emeraldian-theme` + `app.rs`).

### Card A: Readable centred column (item 1)

Acceptance:

1. **Column width.** `[ui] reading_width` exists (default decided by Malte, proposal 80; `0` = full width as today).
   - In reading mode, every prose row is at most `reading_width` columns.
   - The column is centred in the note pane; left and right margins differ by at most 1 column, measured on a `TestBackend` at 200 and 120 columns.
   - At or below `reading_width + 4` columns, the layout matches today.
2. **No blank gutter in reading mode.** The 4-column line-number gutter is not reserved in reading mode when `reading_width > 0`. The built-in editor's no-reflow test still passes, or is consciously replaced with a test that documents the new behaviour.
3. **Focus mode.** F8 uses `reading_width` instead of `FOCUS_WIDTH = 100`, and still hides the panes.
4. **Pictures and drawings** move with the column: a `Picture.indent` test, and the existing `an_embedded_drawing_scrolls_off_the_top_without_spilling` still passes.
5. **Outline and find still land.** Outline jump and Ctrl+F reveal put the target row inside the visible pane (existing tests plus one at 200 columns).
6. **Live proof.** Off-screen Kitty (Xvfb recipe above, private XDG, `OBSIDIAN_CLI=off`): `get-text` of `Prose.md` at 176 columns shows lines ≤ 80 columns with equal margins; a screenshot is attached to the card.

### Card B: Obsidian syntax hygiene (item 2)

Acceptance:

1. **Comments.** Reading mode hides `%%inline%%` and multi-line `%%` blocks. Text after a closing `%%` stays and renders. `%%` inside inline code and fenced code stays visible. An unclosed block hides the rest of the note, as in Obsidian. Covered by parser tests in `markdown.rs`.
2. **Block ids.** A trailing ` ^id` (`[A-Za-z0-9-]+`) is hidden at the end of a paragraph or list item, and on its own line after a table, list, quote or callout. A link `[[X#^id]]` still parses.
3. **Hard breaks.** Two trailing spaces and `<br>`/`<br/>` produce a row break inside a paragraph.
4. **Bold-italic.** `***x***` and `___x___` render bold + italic with no visible asterisks.
5. **`<kbd>`.** `<kbd>K</kbd>` renders as a key chip with the tags gone; other HTML stays as it is (out of scope).
6. **Task states.** The parser keeps the task character. `x`/`X` done (struck), `-` cancelled (struck, faint), `>`, `?`, `/`, `!` and any other character get distinct glyphs and are not struck. Glyphs are Nerd Font code points that CaskaydiaMono Nerd Font has (check with `fc-list ':charset=<hex>'`). No U+2610/2611 remains in reading mode.
7. **Live proof.** Off-screen Kitty screen text of the scratch `Showcase.md` contains none of `%%`, `^para1`, `<br>`, `<kbd>`, `*both*`. A screenshot shows the task list with 6 distinct states.

### Card C: `omarchy` theme from `colors.toml` with hot reload (item 3)

Acceptance:

1. **Loading.** `theme = "omarchy"` loads `~/.local/state/omarchy/current/theme/colors.toml`; an env/config override for tests is allowed.
   - Mapping: `background` → page, `lighter_background` → code panel + inline code background, `dark_background`/`darker_background` → sidebars/status, `foreground` → body, `accent` → accent/links/focus, `selection` → tag chip and selection.
   - Muted and faint text are derived by blending `foreground` toward `background`, because `muted` alone is too faint for body-adjacent text.
   - The hues map directly, with a fallback for missing `orange`/`brown`.
2. **Fallback.** A missing or invalid file falls back to the `terminal` theme with a one-line status message, and the app still starts.
3. **Contrast tests.** Fixture copies of catppuccin-latte, gruvbox and tokyo-night `colors.toml` live in the test tree, never read from `/usr/share` in tests.
   - Body text ≥ 7:1 against the page.
   - Links, tag-chip text on the chip, H1 and inline code text on the code panel ≥ 4.5:1.
   - Muted text ≥ 3:1.
4. **Hot reload.** Changing the file's mtime (or `theme.generation`) recolours a running instance within 2 s with no restart and no new dependency (mtime poll in the existing loop). The reading layout refreshes through the `ReadingCache` palette key.
5. **Live proof.** In off-screen Kitty, swap the overridden `colors.toml` between gruvbox and catppuccin-latte. Screenshots before and after show a visible code panel in both, and a readable tag chip on latte.
6. **Malte's config is not changed** by the card; switching `theme` stays his decision.

## 7. Decisions for Malte

1. Default `reading_width` (80 / 88 / 100), and whether wide content (tables, diagrams) may break out of the column.
2. Omarchy theme heading hues: Omarchy's Obsidian mapping (H1 red, H2 green, H3 yellow, H4 blue, H5/H6 magenta) or the current emeraldian ramp (accent → blue → cyan → green → yellow → muted).
3. New dependencies: syntect for item 11. None of the first batch needs one.
4. Whether item 12 (real heading sizes, L) is worth its risk now or later.
5. His config has `reading_mode = false`, so notes open in nvim. These improvements pay off when he reads in reading mode; maybe try `reading_mode = true` after batch 1.

## 8. Not verified

- The look on Malte's real Wayland HiDPI Kitty. I tested on Xvfb at 96 dpi with font size 13; the colours, glyphs and layout are identical.
- The live recolouring of indexed colours on a real Omarchy theme switch (inferred from how terminals resolve indexed colours).
- Whether the real `Ctrl+]` key toggles the outline; it did not work through `kitten send-key` in my run.
- How often Malte's vault uses note embeds, footnotes, callouts or custom task states (`~/Vault` was off limits).
- Shift+drag copy behaviour under mouse capture.
- Effort figures are estimates from reading the code, not measured.

## Sources

[1] https://sw.kovidgoyal.net/kitty/text-sizing-protocol — Kitty: The text sizing protocol
[2] https://sw.kovidgoyal.net/kitty/graphics-protocol — Kitty: Terminal graphics protocol
[3] https://sw.kovidgoyal.net/kitty/underlines — Kitty: Colored and styled underlines
[4] https://sw.kovidgoyal.net/kitty/kittens/hints — Kitty: Hints kitten
[5] https://sw.kovidgoyal.net/kitty/conf — Kitty: kitty.conf
[7] https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda — Egmont Koblinger: Hyperlinks in terminal emulators (OSC 8)
[9] https://sw.kovidgoyal.net/kitty/clipboard — Kitty: Copying all data types to the clipboard (OSC 5522 / OSC 52)
[12] https://github.com/ratatui/ratatui-image — ratatui-image README
[13] https://github.com/MeanderingProgrammer/render-markdown.nvim — render-markdown.nvim README (MeanderingProgrammer)
[14] https://github.com/OXY2DEV/markview.nvim — markview.nvim README (OXY2DEV)
[15] https://github.com/charmbracelet/glamour/blob/main/styles/dark.json — glamour styles/dark.json
[16] https://github.com/charmbracelet/glamour — glamour README (charmbracelet)
[17] https://github.com/hoonkim/mdcat — hoonkim/mdcat: Markdown viewer for kitty (OSC 66 + graphics)
[18] https://docs.rs/crate/mdcat/0.27.1 — mdcat 0.27.1 (swsnr) on docs.rs
[19] https://github.com/Textualize/frogmouth — Frogmouth README (Textualize)
[21] https://help.obsidian.md/callouts — Obsidian Help: Callouts
[22] https://help.obsidian.md/Links+and+attachments/Internal+links — Obsidian Help: Internal links
[23] https://help.obsidian.md/embeds — Obsidian Help: Embed files
[24] https://help.obsidian.md/Editing+and+formatting/Basic+formatting+syntax — Obsidian Help: Basic formatting syntax
[25] https://help.obsidian.md/Hacking/Properties — Obsidian Help: Properties
[27] https://help.obsidian.md/plugins/page-preview — Obsidian Help: Page preview
[28] https://obsidian.md/help/folding — Obsidian Help: Folding
[29] https://help.obsidian.md/Plugins/Find+and+replace — Obsidian Help: Find and replace
[31] https://obsidian.md/help/settings — Obsidian Help: Settings
[32] https://forum.obsidian.md/t/how-to-make-increase-the-line-length/100647 — Obsidian Forum: How to make increase the line length? (readable line length CSS)
[33] https://github.com/kepano/obsidian-minimal/blob/master/src/css/main.css — kepano/obsidian-minimal: src/css/main.css
[35] https://forum.obsidian.md/t/css-for-showing-heading-levels-in-gutter/72244/2 — Obsidian Forum: CSS for showing heading levels in gutter
[36] https://publish.obsidian.md/hub/02+-+Community+Expansions/02.05+All+Community+Expansions/Themes/ITS+Theme — Obsidian Hub: ITS Theme
[37] https://publish.obsidian.md/slrvb-docs/ITS+Theme/Callout+Adjustments — SlRvb docs: ITS Theme Callout Adjustments
[38] https://github.com/folke/snacks.nvim/blob/main/docs/image.md — snacks.nvim docs/image.md
