import React,{Component,useEffect,type ReactNode} from 'react';
import {invoke} from '@tauri-apps/api/core';

declare global {interface Window {cadpadStartup?:{fail:(error:unknown)=>void;ready:()=>void}}}
function Ready({children}:{children:ReactNode}){useEffect(()=>window.cadpadStartup?.ready(),[]);return children;}
export class StartupBoundary extends Component<{children:ReactNode},{error:Error|null;recovering:boolean;recoveryError:string}> {
  state={error:null as Error|null,recovering:false,recoveryError:''};
  static getDerivedStateFromError(error:unknown){return {error:error instanceof Error?error:new Error(String(error))};}
  async fresh(){
    this.setState({recovering:true,recoveryError:''});
    try{await invoke('new_scratchpad');window.location.reload();}
    catch(error){this.setState({recovering:false,recoveryError:error instanceof Error?error.message:String(error)});}
  }
  render(){return this.state.error?<main className="cadpad-startup" role="alert"><h1>Cadpad couldn’t open this workspace</h1><p>Reload to try again, or start a new scratchpad. Existing projects stay intact.</p><button onClick={()=>window.location.reload()}>Reload Cadpad</button> <button disabled={this.state.recovering} onClick={()=>void this.fresh()}>{this.state.recovering?'Opening…':'New scratchpad'}</button><details><summary>Error details</summary><pre>{this.state.error.message}{this.state.error.stack?`\n${this.state.error.stack}`:''}</pre></details>{this.state.recoveryError&&<p>{this.state.recoveryError}</p>}</main>:<Ready>{this.props.children}</Ready>;}
}
