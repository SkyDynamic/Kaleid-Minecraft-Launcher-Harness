import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles/theme.css';
import { App } from './App';
import {
  applyFont,
  applyFontSize,
  applyMotion,
  applyTheme,
  readPrefs,
  resolveTheme,
} from './store/prefs';

// 主题在 React 挂载之前就写死到 <html>，否则浅色用户会先闪一下深色底。
// 字号同理：挂载后再缩放会让整页布局跳一下。
const bootPrefs = readPrefs();
applyTheme(resolveTheme(bootPrefs.theme));
applyMotion(bootPrefs);
applyFontSize(bootPrefs);
void applyFont(bootPrefs);

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
