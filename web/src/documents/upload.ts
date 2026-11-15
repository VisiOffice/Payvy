// Which files the inbox takes, shared by every place a bill can be uploaded.
export const ACCEPT = 'application/pdf,image/png,image/jpeg,image/gif,image/webp,image/tiff';
const MAX_BYTES = 25 * 1024 * 1024;
function isInvoiceFile(file: File): boolean {
  const type = file.type.toLowerCase();
  return type === 'application/pdf' || type.startsWith('image/') || /\.(pdf|png|jpe?g|gif|webp|tiff?)$/i.test(file.name);
}
/** Files this page will take, and a sentence for each one it will not. */
export function chosen(list: File[]): { ok: File[]; problems: string[] } {
  const ok: File[] = [];
  const problems: string[] = [];
  for (const file of list) {
    if (file.size > MAX_BYTES) problems.push(`${file.name} is larger than 25 MB`);
    else if (!isInvoiceFile(file)) problems.push(`${file.name} isn't a PDF or a photo`);
    else ok.push(file);
  }
  return { ok, problems };
}
export function base64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).slice(String(r.result).indexOf(',') + 1));
    r.onerror = () => reject(r.error ?? new Error(`Couldn't read ${file.name}`));
    r.readAsDataURL(file);
  });
}
