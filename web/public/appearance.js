// Apply a saved preference before the app's styles paint. Storage may be blocked.
try {
  const mode = localStorage.getItem('payvy.appearance');
  if (mode === 'light' || mode === 'dark') document.documentElement.dataset.theme = mode;
} catch { /* Follow the system. */ }
