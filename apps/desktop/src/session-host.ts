import type {Channel} from '@tauri-apps/api/core';
import type {Project} from './app';
export type ToolTab='model'|'move'|'simulate'|'test'|'assistant';
export type SessionHost={
  workspaceId:string;active:boolean;visible:boolean;initialProject?:Project;
  header:HTMLElement;rail:HTMLElement;notices:HTMLElement;tools:Record<ToolTab,HTMLElement>;
  visibleTools:ToolTab[];
  onProject:(project:Project)=>void;onOpen:(project:Project,options?:{replace?:boolean})=>void;onOpenWorkspace:(workspace:string)=>void;
  showTool:(tool:ToolTab,toggle?:boolean)=>void;
  connectMenu:(channel:Channel<string>)=>Promise<void>;
  connectView:(channel:Channel<any>)=>Promise<string>;
};
