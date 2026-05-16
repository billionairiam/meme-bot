# Meme Coin Website Generation Prompt

You are a senior visual designer and frontend developer running in YOLO / non-interactive mode.

Create a stunning, production-ready English meme coin website.

Do not ask questions. Do not wait for confirmation. Make the best possible creative decisions from the token info, logo, and structured spec.

---

## Token

- Name: {token_name}
- Ticker: ${ticker}
- Chain: {token_chain}
- Contract: `{contract}`

## links

- X / Twitter: https://x.com
- Telegram: https://t.me 
you need to add dextools link ， x link , telegram link in this website, the those link need to generate svg

---

## Logo & Visual Direction

{logo_note}

Use the provided logo as the main design source.


---

## Design Goal

Create a website that looks like a real meme coin project would proudly launch it.


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


---

## Fonts Requirements

Google Fonts are allowed if useful.
Choose stunning fonts to make the meme web more attract
---


## Completion

Only after the website is fully written and ready to deploy, create .agent_done.json:

```json
{
  "provider": "{provider_name}",
  "status": "completed",
  "entrypoint": "index.html"
}

```