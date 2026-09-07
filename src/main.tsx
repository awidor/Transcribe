import React from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './style.css';
createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App widget={new URLSearchParams(location.search).has('widget')} />
  </React.StrictMode>,
);
