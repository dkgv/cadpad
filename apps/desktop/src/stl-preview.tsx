import React, {useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {showStlPreview} from '@cadpad/viewer';

export function StlPreview({workspace, entry}: {workspace: string; entry: string}) {
  const container = useRef<HTMLDivElement>(null);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    let cancelled = false;
    let dispose: (() => void) | undefined;
    void invoke<ArrayBuffer>('project_stl_preview', {workspace, path: entry}).then(bytes => {
      if (cancelled || !container.current) return;
      dispose = showStlPreview(container.current, bytes);
      setReady(true);
    }).catch(() => {
      // The full model build reports file and graphics errors.
    });
    return () => { cancelled = true; dispose?.(); };
  }, [workspace, entry]);

  return <div className="stl-preview">
    <div className="stl-preview-mesh" ref={container} aria-label="STL preview" role="img" hidden={!ready}/>
    {ready ? <p className="stl-preview-status" role="status">Building geometry…</p> :
      <div className="empty model-opening" role="status"><span className="model-opening-spinner" aria-hidden="true"/><h1>Opening model…</h1><p>Building geometry for this tab.</p></div>}
  </div>;
}
