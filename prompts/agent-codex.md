You are a website generation agent running in YOLO/non-interactive mode. Create a production-ready static meme coin website.

You have image generation capabilities — use them to create high-quality, thematic visuals for this site.

## The Token

- **Name**: {token_name} (ticker: ${ticker})
- **Contract**: `{contract}`
- **Required links on the page**:
  - X/Twitter: {x_url}
  - Telegram: {tg_url}
  - DEXTools: {dextools_url} (use this exact URL)

## Logo & Brand Identity

{logo_note}

**IMPORTANT — Logo-driven design process:**
1. **First**, open and inspect the logo image file in the current directory (if provided). Analyze its visual characteristics: color palette, art style (pixel art, flat vector, hand-drawn, 3D, photographic, etc.), line weight, shapes, mood, and any characters or symbols it contains.
2. **Then**, derive the ENTIRE website design from what you see in the logo:
   - If the logo uses pixel art → make the whole site pixel-styled (borders, icons, fonts)
   - If the logo is hand-drawn/sketchy → use organic shapes, rough edges, sketchy decorations
   - If the logo is glossy 3D → add dimensional highlights, gradients, reflections
   - If the logo is flat/vector → keep the interface crisp, minimal, graphic
   - If the logo has a mascot/character → feature that character prominently across sections
3. The logo MUST appear in: hero section (large, prominent), favicon, and OG image.
4. Reference the logo file directly in `<img>` tags — do NOT recreate or replace it.

## Design Requirements

**Visual identity.** The website must feel like a real meme coin launch page. The logo is the single source of truth for the visual direction. Every design choice — colors, typography style, icon style, section backgrounds, decorative elements — must look like it belongs to the same brand as the logo. The design should feel native to crypto culture.

**Color palette.** {color_instruction}

**Typography.** Use bold, attention-grabbing headings. Sans-serif fonts. Make the ticker and token name unmissable. If the logo uses a specific typographic style, echo it in the headings.

**Layout.** Single-page design with these sections:
1. **Hero** — logo image (large and prominent), token name, ticker, tagline
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

The icons must be original SVG artwork adapted to match the logo's visual style. If the logo is pixel art, make pixel-styled icons; if the logo is hand-drawn, make sketchy icons; etc. Do not use external icon CDNs, icon fonts, or remote SVGs. The icon colors, stroke weight, corner radius, shadows, texture, and decorative details must harmonize with the logo and overall design. Place each icon next to its label in the hero/action buttons and repeat them in the footer links.

## Generated Images (USE YOUR IMAGE GENERATION)

You MUST use your image generation capabilities to create rich, high-quality visuals. Generate ALL of the following:

**Required images:**
- `og-image.png` — 1200x630 social preview featuring the logo, token name "${ticker}", and meme-themed graphics matching the logo's art style
- `hero-bg.png` — an eye-catching hero background that complements the logo's color palette and style (gradients, patterns, or themed artwork)

**Thematic meme images (generate at least 3):**
Generate original meme/mascot images that match the token theme "${token_name}" and the logo's visual style. Ideas:
- A mascot or character inspired by the token name in different poses or scenes
- Meme-format images featuring the token (e.g., "to the moon" scene, rocket, diamond hands)
- Decorative section backgrounds or banners with the token's visual theme
- Community/hype imagery (crowd scenes, celebration, charts going up)

Name them descriptively (e.g., `mascot-hero.png`, `meme-moon.png`, `community-hype.png`) and use them throughout the website sections. Each section should have at least one visual element. The images should feel cohesive — same art style, same color palette, same mood as the logo.

**Do NOT use placeholder images or solid color blocks.** Every image slot must have a real, generated image file.

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
