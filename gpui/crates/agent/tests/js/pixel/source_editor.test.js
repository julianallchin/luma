// Visual review of the clip graph sheet: a color chase and Aim's simpler
// position and circle presets.
fixture({seconds:20,rig:4,window:[1400,1000]});
for (const preset of ["Chase","Position","Circle"]) {
  test(`source editor: ${preset}`, {fixture:{clips:[{pattern:"source",name:preset,start:1,end:5,preset}]}},()=>{
    nav.trackEditor("Test Venue","Aurora");nav.expand();nav.stageOff();
    nav.step("clip","card",preset);
    until("inputs",s=>s.find({role:"card",label:"Clip graph"}));
    nav.widenGraph();
    if(preset==="Circle") {
      // The yaw's curve is a chip on the aim; open, its strip.
      app.click(nav.inCard("Aim 1","button","Expand Curve 1"));
      const strip=nav.inCard("Aim 1","card","Curve 1 strip").bounds;
      const points=app.snapshot().findAll({role:"slider"}).filter(n=>/^Curve 1 point [0-9]+$/.test(n.label));
      const heights=points.map(n=>n.bounds.y);
      assert(Math.max(...heights)-Math.min(...heights)>strip.height/2,"the motion curve should be readable, not flattened by its bounds");
      // The last bound row pans into view.
      nav.inCard("Aim 1","row","High");
    }
    const shot=app.screenshot();image.keep(shot,`sources/${preset.toLowerCase()}`);
    const content=image.stats(app.screenshot({node:app.snapshot().find({role:"card",label:"Clip graph"})}));
    assert(content.max>content.min,"the source editor is drawn");
  });
}
