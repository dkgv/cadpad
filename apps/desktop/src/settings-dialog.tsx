import React,{useEffect,useState} from 'react';
import {Icon} from './ui';

export type CliDetection={backend:string;installed:boolean;authenticated:boolean;path?:string;detail:string};
export type AgentSettings={provider:string;backend?:string;model:string;configured:boolean;credential_storage:string;cli?:CliDetection[];bridge_available?:boolean};
export type ModelChoice={id:string;label:string};
export type McpSettings={config:unknown;bridge_installed:boolean;sessions:{id:string;client_name:string;connected:boolean;calls:number}[]};

const providers:[string,string][]=[
  ['claude_cli','Claude Code CLI'],
  ['codex_cli','Codex CLI'],
  ['cursor_cli','Cursor CLI'],
  ['anthropic_api','Anthropic API key'],
];

function StatusChip({tone,children}:{tone:'ok'|'warn'|'bad'|'neutral';children:React.ReactNode}){
  return <span className={`settings-chip is-${tone}`}>{children}</span>;
}

function cliTone(item?:CliDetection):'ok'|'warn'|'bad'|'neutral'{
  if(!item)return 'neutral';
  if(!item.installed)return 'bad';
  if(!item.authenticated)return 'warn';
  return 'ok';
}

function cliLabel(item?:CliDetection){
  if(!item)return 'Checking…';
  if(!item.installed)return 'Not installed';
  if(!item.authenticated)return 'Needs sign-in';
  return 'Ready';
}

type Props={
  open:boolean;
  dialogRef:React.RefObject<HTMLDialogElement|null>;
  agentBackend:string;
  modelId:string;
  apiKey:string;
  savingProvider:boolean;
  cleaningCache:boolean;
  providerSettings:AgentSettings|null;
  mcpSettings:McpSettings|null;
  models:ModelChoice[]|null;
  onClose:()=>void;
  onBackend:(value:string)=>void;
  onModel:(value:string)=>void;
  onApiKey:(value:string)=>void;
  onSave:()=>void;
  onForgetKey:()=>void;
  onRecheck:()=>void;
  onCleanCache:()=>void;
  onExternalOpen:(open:boolean)=>void;
  onDisconnect:(sessionId:string)=>void;
  onError:(message:string)=>void;
};

export function SettingsDialog({
  open,dialogRef,agentBackend,modelId,apiKey,savingProvider,cleaningCache,providerSettings,mcpSettings,models,
  onClose,onBackend,onModel,onApiKey,onSave,onForgetKey,onRecheck,onCleanCache,onExternalOpen,onDisconnect,onError,
}:Props){
  const [copied,setCopied]=useState(false);
  const [customModel,setCustomModel]=useState(false);
  const cli=providerSettings?.cli?.find(item=>item.backend===agentBackend);
  const api=agentBackend==='anthropic_api';
  const bridgeOk=!!providerSettings?.bridge_available;
  const saveDisabled=savingProvider||(api?!modelId.trim():!bridgeOk);
  const mcpJson=mcpSettings?JSON.stringify(mcpSettings.config,null,2):'';

  useEffect(()=>{if(!copied)return;const timer=window.setTimeout(()=>setCopied(false),1600);return()=>window.clearTimeout(timer);},[copied]);
  useEffect(()=>{setCustomModel(false);},[agentBackend]);
  const catalog=models??[];
  const known=!!modelId&&catalog.some(model=>model.id===modelId);
  const showCustom=customModel||(!!modelId&&models!==null&&!known);
  const selectValue=showCustom?'__custom__':modelId;
  const modelLabel=catalog.find(model=>model.id===modelId)?.label||modelId;
  function chooseModel(value:string){
    if(value==='__custom__'){setCustomModel(true);return;}
    setCustomModel(false);
    onModel(value);
  }

  async function copyConfig(){
    if(!mcpJson)return;
    try{
      await navigator.clipboard.writeText(mcpJson);
      setCopied(true);
    }catch(error){
      onError(error instanceof Error?error.message:String(error));
    }
  }

  return <dialog ref={dialogRef} className="settings-dialog" onCancel={onClose} onClose={onClose} onClick={event=>{if(event.target===event.currentTarget)onClose();}}>
    <div className="settings-sheet">
      <div className="settings-heading">
        <div>
          <p className="eyebrow">Preferences</p>
          <h2>Settings</h2>
        </div>
        <button className="icon-button" aria-label="Close settings" onClick={onClose}><Icon name="close"/></button>
      </div>
      <div className="settings-body">
        {open&&<section className="provider-settings settings-section">
          <header className="settings-section-header">
            <strong>Agent</strong>
            <p>Use a local CLI with your existing Claude, ChatGPT/Codex, or Cursor subscription, or an Anthropic API key. CLIs talk to Cadpad through MCP.</p>
          </header>
          <label>Provider
            <select value={agentBackend} onChange={event=>onBackend(event.target.value)}>
              {providers.map(([value,label])=><option key={value} value={value}>{label}</option>)}
            </select>
          </label>
          <label>Model
            <select aria-label="Model" value={selectValue} onChange={event=>chooseModel(event.target.value)}>
              {!api&&<option value="">Provider default</option>}
              {api&&!modelId&&<option value="">Choose a model</option>}
              {catalog.map(model=><option key={model.id} value={model.id}>{model.label}</option>)}
              {!!modelId&&selectValue===modelId&&!known&&<option value={modelId}>{modelId}</option>}
              <option value="__custom__">Custom model ID…</option>
            </select>
          </label>
          {models===null&&<p className="muted">Loading models…</p>}
          {(api||showCustom)&&<label>Model ID<input aria-label="Model ID" value={modelId} placeholder={api?'Model ID from your API account':'Exact model ID'} onChange={event=>onModel(event.target.value)} autoComplete="off"/></label>}
          <p className="muted">{api?'This model is sent with each Anthropic request.':'New assistant turns use this model. Provider default keeps the CLI’s own choice.'}</p>
          {!api&&<div className="settings-status" role="status">
            <div className="settings-status-row">
              <span className="settings-status-label">CLI</span>
              <StatusChip tone={cliTone(cli)}>{cliLabel(cli)}</StatusChip>
            </div>
            {cli&&<p className="muted">{cli.detail}{cli.path?` · ${cli.path}`:''}</p>}
            <div className="settings-status-row">
              <span className="settings-status-label">MCP bridge</span>
              <StatusChip tone={bridgeOk?'ok':'bad'}>{bridgeOk?'Available':'Unavailable'}</StatusChip>
            </div>
            <p className="muted">{bridgeOk?'Cadpad configures this connection automatically when launching your agent.':'Modeling requires this connection. Restart Cadpad through the desktop launcher to restore it.'}</p>
            <div className="settings-inline-actions">
              <button type="button" disabled={savingProvider} onClick={onRecheck}>Recheck connection</button>
            </div>
          </div>}
          {api&&<div className="settings-field-group">
            <label>API key<input type="password" value={apiKey} placeholder={providerSettings?.configured?'Saved key · leave blank to keep':'Enter API key'} onChange={event=>onApiKey(event.target.value)} autoComplete="off"/></label>
            {providerSettings?.configured&&<div className="settings-inline-actions">
              <button type="button" className="danger-button" disabled={savingProvider} onClick={onForgetKey}>Remove saved key</button>
            </div>}
          </div>}
        </section>}
        <details className="connection-settings">
          <summary>Storage</summary>
          <p>Remove unused caches and trim stored display data. Models, history, recordings and active exports are preserved. Display data can be restored when needed.</p>
          <div className="settings-row-action">
            <button type="button" disabled={cleaningCache} onClick={onCleanCache}>{cleaningCache?'Cleaning cache…':'Clean cache'}</button>
          </div>
        </details>
        <details className="connection-settings" onToggle={event=>onExternalOpen(event.currentTarget.open)}>
          <summary>External agents</summary>
          <p>Launch Cadpad first, then add this configuration to your MCP client.</p>
          {mcpSettings&&<>
            <div className="settings-code-panel">
              <div className="settings-code-heading">
                <span>MCP client configuration</span>
                <button type="button" onClick={()=>void copyConfig()}>{copied?'Copied':'Copy'}</button>
              </div>
              <textarea readOnly aria-label="MCP client configuration" value={mcpJson}/>
            </div>
            {!mcpSettings.bridge_installed&&<p className="settings-warning">The MCP bridge executable is missing. Restart through the desktop launcher.</p>}
            {!!mcpSettings.sessions.length&&<ul className="external-session-list">
              {mcpSettings.sessions.map(session=><li className="external-session" key={session.id}>
                <span>{session.client_name}<small>{session.connected?'Connected':'Disconnected'}{session.calls?` · ${session.calls} calls`:''}</small></span>
                {session.connected&&<button type="button" onClick={()=>onDisconnect(session.id)}>Disconnect</button>}
              </li>)}
            </ul>}
          </>}
        </details>
      </div>
      <div className="settings-footer">
        {api&&providerSettings?.configured&&<span className="muted">API key saved in the system keychain</span>}
        {!api&&providerSettings?.configured&&<span className="muted">{modelId?`Using ${modelLabel}`:'Using the CLI default model'}</span>}
        <button className="primary-button" disabled={saveDisabled} onClick={onSave}>{savingProvider?'Saving…':'Save settings'}</button>
      </div>
    </div>
  </dialog>;
}
