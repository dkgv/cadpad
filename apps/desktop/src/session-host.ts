export type MessageSink<T>={onmessage:(message:T)=>void};
import type {Project} from './app';
import type {Entity} from '@cadpad/viewer';
export type SelectionContext={workspace:string;revision:string;entities:Entity[];omitted:Set<string>;capture:()=>string|null;toggleOmitted:(entity:Entity)=>void;clear:()=>void};
export type ToolTab='model'|'move'|'simulate'|'test'|'assistant';
export type SessionHost={
  workspaceId:string;modelEntry?:{entry:string;builder:string};onOpenModel?:(entry:string,builder:string)=>Promise<void>;
  /** Focused canvas session — owns menus, save, motion panels, and dialogs. */
  active:boolean;
  /** Sticky shared shell — owns header, rail, Explorer, and Assistant. */
  chrome:boolean;
  visible:boolean;initialProject?:Project;
  /** Stable project id for assistant chats across model tabs. */
  projectWorkspace:string;
  /** Workspace of the focused canvas tab (Explorer / agent edits follow this). */
  focusWorkspace:string;
  focusModelEntry?:{entry:string;builder:string};
  /** Model chosen in Explorer that is still opening. */
  openingModel?:{entry:string;builder:string};
  focusSelection?:SelectionContext;
  onSelection?:(context:SelectionContext)=>void;
  header:HTMLElement;rail:HTMLElement;notices:HTMLElement;tools:Record<ToolTab,HTMLElement>;
  visibleTools:ToolTab[];
  onProject:(project:Project)=>void;onOpen:(project:Project,options?:{replace?:boolean})=>void;onOpenWorkspace:(workspace:string)=>void;
  showTool:(tool:ToolTab,toggle?:boolean)=>void;
  connectMenu:(channel:MessageSink<string>)=>Promise<void>;
  connectView:(channel:MessageSink<any>)=>Promise<string>;
};
