import React,{useEffect,useId,useRef} from 'react';
import {Icon} from './ui';

/** Modal confirmation dialog for model-reverting or destructive actions. */
export function ConfirmDialog({
  title,
  message,
  confirmLabel='Confirm',
  cancelLabel='Cancel',
  busy=false,
  onConfirm,
  onCancel,
}:{
  title:string;
  message:string;
  confirmLabel?:string;
  cancelLabel?:string;
  busy?:boolean;
  onConfirm:()=>void;
  onCancel:()=>void;
}){
  const dialog=useRef<HTMLDialogElement>(null),titleId=useId(),descriptionId=useId();
  useEffect(()=>{
    const node=dialog.current;if(!node)return;
    if(!node.open)node.showModal();
    return()=>{if(node.open)node.close();};
  },[]);
  return <dialog
    ref={dialog}
    className="app-alert-dialog confirm-dialog"
    aria-labelledby={titleId}
    aria-describedby={descriptionId}
    onCancel={event=>{event.preventDefault();if(!busy)onCancel();}}
    onClick={event=>{if(event.target===event.currentTarget&&!busy)onCancel();}}
  >
    <header>
      <h2 id={titleId}>{title}</h2>
      <button type="button" className="icon-button" aria-label={cancelLabel} disabled={busy} onClick={onCancel}><Icon name="close" size={16}/></button>
    </header>
    <div className="app-alert-body">
      <p id={descriptionId}>{message}</p>
    </div>
    <footer>
      <button type="button" disabled={busy} onClick={onCancel}>{cancelLabel}</button>
      <button type="button" className="primary-button" disabled={busy} onClick={onConfirm}>{confirmLabel}</button>
    </footer>
  </dialog>;
}
