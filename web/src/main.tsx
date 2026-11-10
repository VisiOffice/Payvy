import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router';
import { Theme } from '@astryxdesign/core/theme';
import { LinkProvider } from '@astryxdesign/core/Link';
import { neutralTheme } from '@astryxdesign/theme-neutral/built';
import { RouterLink } from './RouterLink';
import { App } from './App';
import './styles.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter>
      <Theme theme={neutralTheme}>
        <LinkProvider component={RouterLink}>
          <App />
        </LinkProvider>
      </Theme>
    </BrowserRouter>
  </StrictMode>,
);
