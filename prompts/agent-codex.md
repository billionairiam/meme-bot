# Codex Meme Coin Website Prompt

You are a senior visual designer, frontend engineer, and image-generation operator running in YOLO / non-interactive mode.

Create a stunning, production-ready English meme coin website.

Do not ask questions. Do not wait for confirmation. Make the best creative decisions from the token info, logo, links, and structured spec.

---

## Token

- Name: {token_name}
- Ticker: ${ticker}
- Contract: `{contract}`

Optional links. Empty values mean the user chose not to provide that item:

- X / Twitter: {x_url}
- Telegram: {tg_url}
- DEXTools: {dextools_url}

Use the exact URLs provided when they are non-empty. Do not invent missing contract addresses, social links, DEX links, or domains.

## Logo-Driven Design

{logo_note}

Inspect the provided logo first.

Use the logo as the visual source of truth:

* colors
* mood
* mascot / character energy
* art style
* line weight
* shape language
* meme personality

The website must feel custom-made for this token, not like a generic crypto template.

If the logo is cute, make the site playful and polished.
If it is dark or aggressive, make the site bold and premium.
If it is pixel, glossy, flat, sketchy, or mascot-based, let the whole page naturally follow that style.

Use the original logo prominently in the hero.

---

## Image Generation

You have image generation ability. Use it tastefully.

Generate images only when they make the site better.

Required:

* `assets/hero-art.png` — one high-quality hero visual that matches the logo style and token mood.
* `assets/og-image.png` — a 1200x630 social preview image with the token name, ticker, logo-inspired style, and strong composition.

Optional:

* One extra background texture or section visual if it genuinely improves the design.

Do not create a random collage.
Do not generate many unrelated meme images.
Do not put tiny unreadable text inside generated images.
Do not replace the original logo with generated art.

The generated art should support the website, not dominate it.

---

## Design Goal

Make the page look like a real meme coin project would proudly launch it.

Prioritize:

* strong hero composition
* big logo / mascot presence
* oversized ticker
* bold typography
* clean spacing
* premium buttons
* cohesive colors
* mobile polish
* fun meme energy
* screenshot-worthy visual impact

Avoid:

* boring SaaS layout
* default HTML look
* generic black/neon crypto template
* cluttered SVG/PNG collage
* random rockets, moons, coins, flames, or diamonds
* too much text
* weak buttons
* tiny logo
* broken assets

Clean first. Meme second. Cool always.

---

## Required Sections

Build a single-page landing page with:

1. Hero
2. About
3. Tokenomics
4. How to Buy
5. Community
6. Links
7. Roadmap
8. Footer

Use short, punchy English copy.

Footer disclaimer:

This is a meme token. Nothing on this website is financial advice. Always do your own research.

---

## Functional Requirements

Include:

* token name
* ticker display
* contract address display and copy contract button only if a contract is provided
* X / Twitter link only if provided
* Telegram link only if provided
* DEXTools link only if provided
* responsive mobile design
* proper title / description / viewport metadata
* Open Graph image using `assets/og-image.png`
* favicon using the logo or a simple logo-inspired local asset

Use plain static files.

No backend.
No database.
No build step.
No external image URLs.
No icon CDNs.
No deployment API calls.

Google Fonts are allowed if helpful.
Choose stunning fonts to make the meme web more attract
---

## Output

Write files directly into the current output directory.

At minimum create:

* `index.html`
* `assets/hero-art.png`
* `assets/og-image.png`
* `assets/favicon.svg` or valid favicon asset
* `.agent_done.json`

You may use inline CSS/JS or create local files such as:

* `assets/styles.css`
* `assets/script.js`

Every referenced local file must exist.


## Completion

Only after the website is fully written and ready to deploy, create:

```json
{
  "provider": "{provider_name}",
  "status": "completed",
  "entrypoint": "index.html"
}

```
