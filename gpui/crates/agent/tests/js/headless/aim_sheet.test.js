// Aim positions and source-driven offsets share the normal input editor.
fixture({seconds:20,graph_score:{clips:{}},rig:4,window:[1400,1000]});
const node=(role,label)=>until(label,s=>s.find({role,label})).find({role,label});
const clip=()=>Object.values(library.score().clips)[0];
function place(name="Wave") {
  nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
  app.type(node("input","Search presets…"),"aim");
  app.click(node("row",name));
  until("clip persisted",()=>library.query("SELECT count(*) AS n FROM clips")[0].n>0);
  until("aim placed",()=>clip()?.graph==="aim@1");
  node("row","Horizontal offset");
}
function selectIn(row,label) {
  let r=node("row",row).bounds;
  const p=node("card","Clip inputs").bounds;
  if(r.y<p.y+65 || r.y+r.height>p.y+p.height-20){app.scroll({x:p.x+p.width/2,y:p.y+p.height/2},{dy:p.y+100-r.y,steps:5});app.frames(2);r=node("row",row).bounds;}
  return app.snapshot().findAll({role:"select",label}).find(n=>n.bounds.y>=r.y&&n.bounds.y<r.y+r.height);
}
test("Aim uses positions and numeric offset sources",()=>{
  place();
  for(const old of ["Motion","Shape","Amount","Spread","Fan","Alpha"])
    assert(!app.snapshot().find({role:"row",label:old}),`old control ${old}`);
  const fields=app.snapshot().findAll({role:"input"}).map(n=>n.label);
  assert(fields.some(l=>l.startsWith("Direction: Turn ="))&&fields.some(l=>l.startsWith("Direction: Tilt =")),"direction has turn and tilt");
  const direction=node("row","Direction").bounds;
  assert(!app.snapshot().findAll({role:"select"}).some(n=>n.bounds.y>=direction.y&&n.bounds.y<direction.y+direction.height),"new positions are edited directly; clips provide transitions");
  expect(clip().inputs.vertical.type).toBe("time");
  app.click(selectIn("Horizontal offset","Fixed"));app.click(node("button","Noise"));
  until("horizontal source saved",()=>clip().inputs.horizontal.type==="noise");
  expect(clip().inputs.vertical.type).toBe("time");
});
test("Offset hides the base and Replace brings it back",()=>{
  place();const before=clip().inputs.direction;
  app.click(selectIn("Blend","Replace"));app.click(node("button","Offset"));
  until("offset saved",()=>clip().blend_mode==="offset");
  assert(!app.snapshot().find({role:"row",label:"Direction"}),"an offset has no base position");
  expect(clip().inputs.direction).toEqual(before);
  app.click(selectIn("Blend","Offset"));app.click(node("button","Replace"));
  until("replace saved",()=>clip().blend_mode==="replace");node("row","Direction");
});
test("Aim mirrors its offset frame and clears the mirror on a nonspatial axis",()=>{
  place("Position");
  const axes=()=>app.snapshot().findAll({role:"row",label:"Axis"});
  // Position has a fixed spatial offset, so only its offset frame has an axis.
  app.click(selectIn("Axis","Order"));app.click(node("button","X"));
  until("x frame saved",()=>clip().inputs.axis.value.source.kind==="u");
  app.click(selectIn("Mirror","Off"));app.click(node("button","Left–right"));
  until("mirror saved",()=>clip().inputs.axis.value.mirror?.normal[0]===1);
  app.click(selectIn("Axis","X"));app.click(node("button","Random"));
  until("random frame saved",()=>clip().inputs.axis.value.source.kind==="random");
  assert(!clip().inputs.axis.value.mirror,"a nonspatial axis has no mirror");
});


test("motion size edits degrees and its curve has no extra multiplier",()=>{
  place("Circle");
  const before=clip().inputs.vertical;
  app.click(selectIn("Size","Fixed"));
  // Dismiss the source menu and edit the size itself.
  app.key("escape");
  const size=()=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Size ="));
  assert(size(),"Circle exposes its size");
  app.click(size());app.key("secondary-a backspace");app.type(size(),"32");app.key("enter");
  until("size saved",()=>clip().inputs.horizontal.value.gain.value===32);
  expect(clip().inputs.vertical).toEqual(before);
  app.click(node("text","Form"));
  app.key("secondary-z");
  until("size undo",()=>clip().inputs.horizontal.value.gain.value!==32);
  app.click(selectIn("Size","Fixed"));app.click(node("button","Time"));
  until("size animation saved",()=>clip().inputs.horizontal.value.gain.type==="time");
  const rows=app.snapshot().findAll({role:"row"}).map(n=>n.label);
  assert(!rows.includes("Amount"),"motion size has no generic multiplier");
  // Only horizontal and vertical motion have phase, not the nested size curve.
  expect(rows.filter(name=>name==="Phase").length).toBe(2);
});
