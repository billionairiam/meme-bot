# Meme Coin Website Generation Prompt

You are a senior visual designer and frontend developer running in YOLO / non-interactive mode.

Create a stunning, production-ready English meme coin website.

Do not ask questions. Do not wait for confirmation. Make the best possible creative decisions from the token info, logo, and structured spec.

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

---

## Logo & Visual Direction

{logo_note}

Use the provided logo as the main design source.

First inspect the logo image if available. Match the whole website to its:

- colors
- mood
- style
- character energy
- visual personality

The site should feel custom-made for this token, not like a generic crypto template.

Make it look cool, viral, polished, and meme-native.

If the logo is cute, make the site playful and clean.
If the logo is dark or aggressive, make the site bold and premium.
If the logo is pixel, sketchy, glossy, flat, or mascot-based, let the page naturally follow that style.

The logo must be used prominently in the hero.

---

## Design Goal

Create a website that looks like a real meme coin project would proudly launch it.

Prioritize:

- strong hero section
- big logo / mascot presence
- oversized ticker
- bold typography
- clean spacing
- memorable colors
- premium buttons
- mobile responsive layout
- fun meme energy
- polished visual hierarchy

Avoid:

- boring SaaS layout
- default HTML look
- generic black-and-neon crypto template
- too much text
- random decorations
- cluttered SVG collage
- tiny logo
- weak buttons
- broken asset references

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

Do not make financial promises.
Do not invent supply, tax, liquidity, ownership, audit, partnership, or launch claims unless provided.

Include a simple disclaimer in the footer:

This is a meme token. Nothing on this site is financial advice. Do your own research.

---

## Functional Requirements

The site must include:

- Token name
- Ticker display
- Contract address display and copy button only if a contract is provided
- X / Twitter link only if provided
- Telegram link only if provided
- DEXTools link only if provided
- Responsive mobile design
- Local logo usage
- No backend
- No database
- No build step

Write files directly into the current output directory.

At minimum create:

- index.html
- style.css
- script.js
- .agent_done.json

You may create extra local SVG/CSS assets only if they improve the design.

Do not use external images.
Do not use icon CDNs.
Do not reference files that do not exist.

Google Fonts are allowed if useful.
Choose stunning fonts to make the meme web more attract
---

## Completion

Only after the website is fully written and ready to deploy, create:

```json
{
  "provider": "{provider_name}",
  "status": "completed",
  "entrypoint": "index.html"
}
