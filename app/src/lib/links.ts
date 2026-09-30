// External links shared by the header, footer and the explainer pages, the same
// targets as the main safustaking.com site.
export const LINKS = {
  site: "https://safustaking.com",
  feedback: "https://docs.google.com/forms/d/1qD9IrIkfs39Wupaw5y-zvCS46Ppq4JQiIsRgV9D3s80/viewform",
  x: "https://x.com/safu_staking",
  telegram: "https://t.me/+sza3oozmzzJlNzk0",
  github: "https://github.com/mrkanchwala",
  bdIntake: "https://docs.google.com/forms/d/e/1FAIpQLSefYrJjgxzJrHJe59U7BFottI7KrQMwgiq_txmShbxBBmWk9w/viewform",
} as const;

// One page for now: the explainer pages (FAQ, whitepaper, protocols) are the USDC pool's and aren't
// copied here yet.
export type View = "main";
