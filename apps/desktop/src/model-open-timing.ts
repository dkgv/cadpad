export type OpenStage = 'renderer'|'backend'|'buffers'|'draw'|'frame'|'pick';

export type ModelOpenFrame = {ms: number; reusedWorkspace: boolean};

export function measureModelOpen(entry: string, builder: string, started: number, reused: () => boolean) {
  const prefix = builder === '$file' && /\.stl$/i.test(entry) ? 'cadpad.stl' : 'cadpad.model';
  const timings: Partial<Record<OpenStage, number>> = {};
  let frame: ModelOpenFrame | undefined;
  return (stage: OpenStage): ModelOpenFrame | undefined => {
    if (timings[stage] !== undefined) return stage === 'frame' ? frame : undefined;
    const end = performance.now();
    timings[stage] = end - started;
    const name = `${prefix}.open-to-${stage}`;
    if (performance.getEntriesByName(name).length >= 100) performance.clearMeasures(name);
    performance.measure(name, {start: started, end});
    if (stage === 'frame') {
      const rounded = Object.fromEntries(Object.entries(timings).map(([key, ms]) => [key, Math.round(ms)]));
      frame = {ms: rounded.frame, reusedWorkspace: reused()};
      console.info('[cadpad:model-open]', JSON.stringify({entry, builder, reusedWorkspace: frame.reusedWorkspace, elapsedMs: rounded}));
      return frame;
    }
  };
}
