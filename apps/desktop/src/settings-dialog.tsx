import React,{useEffect,useState} from 'react';
import {SearchableModelSelect,type ModelOption} from './model-select';
import {Icon,type IconName} from './ui';

export type CliDetection={backend:string;installed:boolean;authenticated:boolean;path?:string;detail:string};
export type AgentSettings={provider:string;backend?:string;model:string;configured:boolean;credential_storage:string;cli?:CliDetection[];bridge_available?:boolean};
export type CloudSettings={base_url:string;configured:boolean;credential_storage:string};
export type ModelChoice={id:string;label:string};
export type McpSettings={config:unknown;bridge_installed:boolean;sessions:{id:string;client_name:string;connected:boolean;calls:number}[]};

const providers:[string,string][]=[
  ['claude_cli','Claude Code CLI'],
  ['codex_cli','Codex CLI'],
  ['cursor_cli','Cursor CLI'],
  ['opencode_cli','OpenCode CLI'],
  ['antigravity_cli','Antigravity CLI'],
  ['anthropic_api','Anthropic API key'],
  ['openrouter_api','OpenRouter API key'],
];

type SettingsTab='agent'|'cloud'|'storage'|'external';

const settingsTabs:{id:SettingsTab;label:string;icon:IconName}[]=[
  {id:'agent',label:'Agent',icon:'sliders'},
  {id:'cloud',label:'Cloud',icon:'sparkles'},
  {id:'storage',label:'Storage',icon:'folder'},
  {id:'external',label:'External agents',icon:'chat'},
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
  cloudSettings:CloudSettings|null;
  signingInCloud:boolean;
  mcpSettings:McpSettings|null;
  models:ModelChoice[]|null;
  onClose:()=>void;
  onBackend:(value:string)=>void;
  onModel:(value:string)=>void;
  onApiKey:(value:string)=>void;
  onSave:()=>void;
  onForgetKey:()=>void;
  onSignInCloud:()=>void;
  onForgetCloudKey:()=>void;
  onRecheck:()=>void;
  onCleanCache:()=>void;
  onExternalOpen:(open:boolean)=>void;
  onDisconnect:(sessionId:string)=>void;
  onError:(message:string)=>void;
};

export function SettingsDialog({
  open,dialogRef,agentBackend,modelId,apiKey,savingProvider,cleaningCache,providerSettings,cloudSettings,signingInCloud,mcpSettings,models,
  onClose,onBackend,onModel,onApiKey,onSave,onForgetKey,onSignInCloud,onForgetCloudKey,onRecheck,onCleanCache,onExternalOpen,onDisconnect,onError,
}:Props){
  const [copied,setCopied]=useState(false);
  const [customModel,setCustomModel]=useState(false);
  const [tab,setTab]=useState<SettingsTab>('agent');
  const cli=providerSettings?.cli?.find(item=>item.backend===agentBackend);
  const api=agentBackend==='anthropic_api'||agentBackend==='openrouter_api';
  const apiLabel=agentBackend==='openrouter_api'?'OpenRouter':'Anthropic';
  const bridgeOk=!!providerSettings?.bridge_available;
  const saveDisabled=savingProvider||(api?!modelId.trim():!bridgeOk);
  const mcpJson=mcpSettings?JSON.stringify(mcpSettings.config,null,2):'';

  useEffect(()=>{if(!copied)return;const timer=window.setTimeout(()=>setCopied(false),1600);return()=>window.clearTimeout(timer);},[copied]);
  useEffect(()=>{setCustomModel(false);},[agentBackend]);
  // Each opening starts on the primary tab; leaving the dialog releases the live MCP session feed.
  useEffect(()=>{if(!open)setTab('agent');},[open]);
  useEffect(()=>{onExternalOpen(open&&tab==='external');},[open,tab,onExternalOpen]);
  const catalog=models??[];
  const known=!!modelId&&catalog.some(model=>model.id===modelId);
  const showCustom=customModel||(!!modelId&&models!==null&&!known);
  const selectValue=showCustom?'__custom__':modelId;
  function chooseModel(value:string){
    if(value==='__custom__'){setCustomModel(true);return;}
    setCustomModel(false);
    onModel(value);
  }
  const modelOptions:ModelOption[]=[
    ...(!api?[{value:'',label:'Provider default'}]:[]),
    ...(!!modelId&&selectValue===modelId&&!known?[{value:modelId,label:modelId}]:[]),
    ...catalog.map(model=>({value:model.id,label:model.label,detail:model.id===model.label?undefined:model.id})),
    {value:'__custom__',label:'Custom model ID…'},
  ];

  function selectTab(next:SettingsTab){
    setTab(next);
  }

  function handleTabKey(event:React.KeyboardEvent<HTMLButtonElement>,id:SettingsTab){
    const index=settingsTabs.findIndex(item=>item.id===id);
    const next=event.key==='ArrowRight'?index+1:event.key==='ArrowLeft'?index-1:event.key==='Home'?0:event.key==='End'?settingsTabs.length-1:-1;
    if(next<0)return;
    event.preventDefault();
    const target=settingsTabs[(next+settingsTabs.length)%settingsTabs.length];
    selectTab(target.id);
    document.getElementById(`settings-tab-${target.id}`)?.focus();
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
        <h2>Settings</h2>
        <button className="icon-button" aria-label="Close settings" onClick={onClose}><Icon name="close"/></button>
      </div>
      <div className="settings-tabs" role="tablist" aria-label="Settings sections">
        {settingsTabs.map(item=>{
          const selected=tab===item.id;
          return <button key={item.id} type="button" role="tab" id={`settings-tab-${item.id}`} data-settings-tab={item.id}
            aria-selected={selected} aria-controls={`settings-panel-${item.id}`} tabIndex={selected?0:-1}
            onClick={()=>selectTab(item.id)} onKeyDown={event=>handleTabKey(event,item.id)}>
            <Icon name={item.icon} size={15}/><span>{item.label}</span>
          </button>;
        })}
      </div>
      <div className="settings-body">
        <div className="settings-panel" role="tabpanel" id="settings-panel-agent" aria-labelledby="settings-tab-agent" hidden={tab!=='agent'}>
          <section className="provider-settings settings-section">
            <header className="settings-section-header">
              <strong>Agent</strong>
              <p>Use Claude Code, Codex, Cursor, OpenCode, or Antigravity with its configured models and accounts, or an Anthropic or OpenRouter API key. CLIs talk to Cadpad through MCP.</p>
            </header>
            <label>Provider
              <select value={agentBackend} onChange={event=>onBackend(event.target.value)}>
                {providers.map(([value,label])=><option key={value} value={value}>{label}</option>)}
              </select>
            </label>
            <div className="provider-settings-field">Model
              <SearchableModelSelect ariaLabel="Model" value={selectValue} options={modelOptions} placeholder={api?'Choose a model':'Provider default'} className="settings-model-trigger" loading={models===null} onChange={chooseModel}/>
            </div>
            {models===null&&<p className="muted">Loading models…</p>}
            {(api||showCustom)&&<label>Model ID<input aria-label="Model ID" value={modelId} placeholder={api?'Model ID from your API account':'Exact model ID'} onChange={event=>onModel(event.target.value)} autoComplete="off"/></label>}
            <p className="muted">{api?`This model is sent with each ${apiLabel} request.`:'New assistant turns use this model. Provider default keeps the CLI’s own choice.'}</p>
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
              <label>{apiLabel} API key<input type="password" value={apiKey} placeholder={providerSettings?.configured?'Saved key · leave blank to keep':'Enter API key'} onChange={event=>onApiKey(event.target.value)} autoComplete="off"/></label>
              {providerSettings?.configured&&<div className="settings-inline-actions">
                <button type="button" className="danger-button" disabled={savingProvider} onClick={onForgetKey}>Remove saved key</button>
              </div>}
            </div>}
          </section>
        </div>
        <div className="settings-panel" role="tabpanel" id="settings-panel-cloud" aria-labelledby="settings-tab-cloud" hidden={tab!=='cloud'}>
          <section className="provider-settings settings-section">
            <header className="settings-section-header">
              <strong>Cadpad Cloud</strong>
              <p>Turn a photo or screenshot into an editable model from the Cadpad Cloud tab in the sidebar, File → Model from Image…, or an image attached in the Assistant.</p>
            </header>
            <div className="settings-inline-actions">
              <button type="button" className="primary-button" disabled={signingInCloud} onClick={onSignInCloud}>{signingInCloud?'Waiting for browser…':'Sign in with browser'}</button>
              {cloudSettings?.configured&&<button type="button" className="danger-button" disabled={signingInCloud} onClick={onForgetCloudKey}>Sign out</button>}
            </div>
            <p className="muted">{cloudSettings?.configured?'Signed in to Cadpad.':'Opens cadpad.app in your browser to sign in. Cadpad finishes setup once you approve.'}</p>
          </section>
        </div>
        <div className="settings-panel" role="tabpanel" id="settings-panel-storage" aria-labelledby="settings-tab-storage" hidden={tab!=='storage'}>
          <section className="settings-section">
            <header className="settings-section-header">
              <p>Remove unused caches and trim stored display data. Models, history, recordings and active exports are preserved. Display data can be restored when needed.</p>
            </header>
            <div className="settings-row-action">
              <button type="button" disabled={cleaningCache} onClick={onCleanCache}>{cleaningCache?'Cleaning cache…':'Clean cache'}</button>
            </div>
          </section>
        </div>
        <div className="settings-panel" role="tabpanel" id="settings-panel-external" aria-labelledby="settings-tab-external" hidden={tab!=='external'}>
          <section className="settings-section">
            <header className="settings-section-header">
              <p>Launch Cadpad first, then add this configuration to your MCP client.</p>
            </header>
            {!mcpSettings&&<p className="muted">Loading connection details…</p>}
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
          </section>
        </div>
      </div>
      {open&&tab==='agent'&&<div className="settings-footer">
        {api&&providerSettings?.configured&&<span className="muted">{apiLabel} API key saved in the system keychain</span>}
        <button className="primary-button" disabled={saveDisabled} onClick={onSave}>{savingProvider?'Saving…':'Save settings'}</button>
      </div>}
    </div>
  </dialog>;
}
