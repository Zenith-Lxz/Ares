import { createRoot } from 'react-dom/client';
import App from './App';
// ★ xterm.css 必须导入：缺失时 helper textarea 失去透明样式（显示为白框），
//   canvas 布局规则也全部丢失（WebGL 渲染区异常 → 看似黑屏）
import '@xterm/xterm/css/xterm.css';
import './styles/theme.css';

createRoot(document.getElementById('root')!).render(<App />);
