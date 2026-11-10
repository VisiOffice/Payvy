// A small, consistent set for the product's navigation; decorative beside labels.
const paths = {
  inbox: <><path d="M4 4h16v12l-3 4H7l-3-4Z" /><path d="M4 14h5l1 3h4l1-3h5" /></>,
  documents: <><path d="M7 3h7l4 4v14H7Z" /><path d="M14 3v5h4M10 12h5M10 16h5" /></>,
  bills: <><path d="M6 3l3 2 3-2 3 2 3-2v18l-3-2-3 2-3-2-3 2Z" /><path d="M9 9h6M9 13h6" /></>,
  vendors: <><circle cx="9" cy="8" r="3" /><path d="M3 20v-2a6 6 0 0 1 12 0v2M16 5a3 3 0 0 1 0 6M18 15a5 5 0 0 1 3 5" /></>,
  workflows: <><rect x="3" y="3" width="6" height="6" rx="1.5" /><rect x="15" y="15" width="6" height="6" rx="1.5" /><path d="M9 6h6a3 3 0 0 1 3 3v6M6 9v9h9" /></>,
};
export function NavIcon({ name }: { name: keyof typeof paths }) {
  return <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}
