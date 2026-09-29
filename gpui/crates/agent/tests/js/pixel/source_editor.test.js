// Visual review of the clip graph sheet: a color chase and Aim's simpler
// position and circle presets.
fixture({seconds:20,rig:4,window:[1400,1000]});
for (const preset of ["Chase","Position","Circle"]) {
  test(`source editor: ${preset}`, {fixture:{clips:[{pattern:"source",name:preset,start:1,end:5,preset}]}},()=>{
    nav.trackEditor("Test Venue","Aurora");nav.expand();nav.stageOff();
    nav.step("clip","card",preset);
    until("inputs",s=>s.find({role:"card",label:"Clip graph"}));
    if(preset==="Circle") {
      const strip=until("strip",s=>s.find({role:"card",label:"Curve 1 strip"})).find({role:"card",label:"Curve 1 strip"}).bounds;
      const points=app.snapshot().findAll({role:"slider"}).filter(n=>/^Curve 1 point [0-9]+$/.test(n.label));
      const heights=points.map(n=>n.bounds.y);
      assert(Math.max(...heights)-Math.min(...heights)>strip.height/2,"the motion curve should be readable, not flattened by its bounds");
      // The last bound row scrolls into view.
      const panel=app.snapshot().find({role:"card",label:"Clip graph"}).bounds;
      const last=()=>app.snapshot().findAll({role:"row",label:"High"}).sort((a,b)=>b.bounds.y-a.bounds.y)[0];
      app.scroll({x:panel.x+panel.width/2,y:panel.y+panel.height/2},{dy:panel.y+panel.height*0.8-last().bounds.y,steps:5});
      until("the last bound visible",()=>{
        const row=last().bounds;
        return row.y>panel.y && row.y+row.height<panel.y+panel.height;
      });
    }
    const shot=app.screenshot();image.keep(shot,`sources/${preset.toLowerCase()}`);
    const content=image.stats(app.screenshot({node:app.snapshot().find({role:"card",label:"Clip graph"})}));
    assert(content.max>content.min,"the source editor is drawn");
  });
}
