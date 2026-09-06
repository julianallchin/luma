// Stackless traversal of the shared, two-sided opaque stage triangle BVH.
struct VisibilityNode {
    lo: vec3<f32>, escape: u32,
    hi: vec3<f32>, leaf: u32,
    a: vec4<f32>, b: vec4<f32>, c: vec4<f32>,
};
@group(3) @binding(11) var<storage, read> visibility_nodes: array<VisibilityNode>;

fn stage_visibility(origin: vec3<f32>, endpoint: vec3<f32>) -> f32 {
    let delta = endpoint-origin;
    let distance = length(delta);
    if distance < 0.004 { return 1.0; }
    let direction = delta/distance;
    // Signed epsilon avoids NaNs for parallel rays on an AABB face.
    let safe = select(vec3<f32>(-1e-8), vec3<f32>(1e-8), direction >= vec3<f32>(0.0));
    let inverse = 1.0/select(safe, direction, abs(direction)>vec3<f32>(1e-8));
    var index = 0u;
    let count = arrayLength(&visibility_nodes);
    while index < count {
        let node = visibility_nodes[index];
        if node.escape == 0u { break; } // disabled/empty scene sentinel
        let t0 = (node.lo-origin)*inverse;
        let t1 = (node.hi-origin)*inverse;
        let near = min(t0,t1);
        let far = max(t0,t1);
        if max(max(near.x,near.y),max(near.z,0.002)) >
            min(min(far.x,far.y),min(far.z,distance-0.002)) {
            index = node.escape;
            continue;
        }
        if node.leaf != 0u {
            let p = cross(direction,node.c.xyz);
            let det = dot(node.b.xyz,p);
            if abs(det)>1e-8 {
                let s = origin-node.a.xyz;
                let u = dot(s,p)/det;
                let q = cross(s,node.b.xyz);
                let v = dot(direction,q)/det;
                let t = dot(node.c.xyz,q)/det;
                if u>=0.0 && v>=0.0 && u+v<=1.0 && t>0.002 && t<distance-0.002 {
                    return 0.0;
                }
            }
        }
        index += 1u;
    }
    return 1.0;
}
