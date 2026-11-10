import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router';
import { AppearanceProvider } from './Appearance';
import { LinkProvider } from '@astryxdesign/core/Link';
import { ToastViewport } from '@astryxdesign/core/Toast';
import { RouterLink } from './RouterLink';
import { App } from './App';
import './styles.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter>
      <AppearanceProvider>
        <LinkProvider component={RouterLink}>
          <ToastViewport position="bottomEnd">
            <App />
          </ToastViewport>
        </LinkProvider>
      </AppearanceProvider>
    </BrowserRouter>
  </StrictMode>,
);
