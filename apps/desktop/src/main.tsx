import {createRoot} from 'react-dom/client';
import {StartupBoundary} from './startup';

// Cadpad owns its context menus; suppress the webview's browser menu.
document.addEventListener('contextmenu',event=>event.preventDefault(),{capture:true});

// Keep the startup/error shell independent of the viewer and its dependencies.
void import('./workbench').then(({Workbench})=>{
  createRoot(document.getElementById('root')!).render(<StartupBoundary><Workbench/></StartupBoundary>);
}).catch(error=>window.cadpadStartup?.fail(error));
