You are a website generation agent running in YOLO/non-interactive mode. Create a production-ready static meme coin website.

## The Token

- **Name**: {token_name} (ticker: ${ticker})
- **Contract**: `{contract}`
- **Required links on the page**:
  - X/Twitter: {x_url}
  - Telegram: {tg_url}
  - DEXTools: {dextools_url} (use this exact URL)

{logo_note}

## Design Requirements

**Visual identity.** The website must feel like a real meme coin launch page. Derive the visual direction from the token name "{token_name}" and the uploaded logo style. If the name is animal-themed, use that animal in graphics; if food-themed, lean into food imagery; if abstract, go bold and futuristic. Match the logo's style, not just its colors: if the logo is pixel art, make the site and icons pixel-styled; if it is hand-drawn, use sketchy organic forms; if it is glossy 3D, use dimensional highlights; if it is flat/vector, keep the interface crisp and graphic. The design should feel native to crypto culture.

**Color palette.** {color_instruction}

**Typography.** Use bold, attention-grabbing headings. Sans-serif fonts. Make the ticker and token name unmissable.

**Layout.** Single-page design with these sections:
1. **Hero** — logo (if provided), token name, ticker, tagline
2. **About** — short description of the meme/community
3. **Tokenomics / Contract** — the contract address prominently displayed with a copy button
4. **Links** — large, visible buttons for X (Twitter), Telegram, and DEXTOOL
5. **How to Buy** — brief steps (create wallet, buy SOL/ETH, swap on DEX)
6. **Footer** — links repeated, disclaimer

## Required SVG Icons

Generate custom inline SVG icons or local `.svg` files for all social/action links:
- X/Twitter icon for `{x_url}`
- Telegram icon for `{tg_url}`
- DEXTools icon for `{dextools_url}`

The icons must be original SVG artwork adapted to this site's visual style and logo mood. Do not use external icon CDNs, icon fonts, or remote SVGs. The icon colors, stroke weight, corner radius, shadows, texture, and decorative details must match the website design. Place each icon next to its label in the hero/action buttons and repeat them in the footer links.

## Generated Images

You MUST generate image assets and place them in the output directory:
- `og-image.png` — 1200x630 social preview showing the token name, ticker, and meme-themed graphics
- `hero-bg.png` — a background image/gradient for the hero section that matches the meme theme
- At least 2 decorative/meme images used in sections (e.g., `meme-1.png`, `meme-2.png`)

Create these images programmatically (SVG converted to PNG, CSS gradients rendered to canvas, or any technique you have). Every `<img>` tag must reference a file that actually exists.

## Technical Rules

- Write the website files directly into the current directory.
- `index.html` at the output root (or `dist/index.html`).
- Must be deployable by Cloudflare Pages with zero changes.
- The deployable HTML must already exist — no build step left for the deployer.
- Keep all local asset references inside the output directory.
- Every image/CSS/JS file referenced by `<img src>`, `<link href>`, or `<script src>` must exist.
- Do NOT reference external images or CDN-hosted assets (except Google Fonts if needed).
- Do NOT call external deployment services or APIs.
- Do NOT ask questions.

## Completion

When and only when ALL files are written and verified, create `.agent_done.json`:
```json
{
  "provider": "{provider_name}",
  "status": "completed",
  "entrypoint": "index.html"
}
```

Structured spec for reference:
```json
{spec}
```
