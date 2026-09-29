// Visual review of the shared source editor and Aim's simpler position preset.
fixture({seconds:20,rig:4,window:[1400,1000]});
for (const [form,preset,card] of [["color@1","Chase","Color"],["aim@1","Position","Aim"],["aim@1","Circle","Aim"]]) {
  test(`source editor: ${preset}`, {fixture:{clips:[{pattern:"source",name:preset,start:1,end:5,preset:[form,preset]}]}},()=>{
    nav.trackEditor("Test Venue","Aurora");nav.expand();nav.stageOff();
    nav.step("clip","card",card);
    until("inputs",s=>s.find({role:"card",label:"Clip inputs"}));
    if(preset==="Circle") {
      const strip=app.snapshot().find({role:"card",label:"Horizontal offset strip"}).bounds;
      const points=app.snapshot().findAll({role:"slider"}).filter(n=>/^Horizontal offset point [0-9]+$/.test(n.label));
      const heights=points.map(n=>n.bounds.y);
      assert(Math.max(...heights)-Math.min(...heights)>strip.height/2,"the motion curve should be readable, not flattened by its size control");
      const panel=app.snapshot().find({role:"card",label:"Clip inputs"}).bounds;
      const size=app.snapshot().find({role:"row",label:"Size"}).bounds;
      app.scroll({x:panel.x+panel.width/2,y:panel.y+panel.height/2},{dy:panel.y+panel.height*0.8-size.y,steps:5});
      until("motion size visible",s=>{
        const row=s.find({role:"row",label:"Size"}).bounds;
        return row.y>panel.y && row.y+row.height<panel.y+panel.height;
      });
    }
    const shot=app.screenshot();image.keep(shot,`sources/${preset.toLowerCase()}`);
    const content=image.stats(app.screenshot({node:app.snapshot().find({role:"card",label:"Clip inputs"})}));
    assert(content.max>content.min,"the source editor is drawn");
  });
}
