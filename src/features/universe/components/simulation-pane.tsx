import { Grid, OrbitControls } from "@react-three/drei";
import { Canvas } from "@react-three/fiber";

export function SimulationPane() {
	return (
		<div className="w-full h-full bg-black">
			<Canvas camera={{ position: [0, 10, 25], fov: 50 }}>
				<color attach="background" args={["#1a1a1a"]} />
				<ambientLight intensity={0.5} />
				<directionalLight position={[10, 10, 5]} intensity={1} />
				<Grid
					infiniteGrid
					fadeDistance={50}
					fadeStrength={1}
					cellColor="#81a1c1"
					sectionColor="#4d707a"
					sectionSize={3}
					cellSize={1}
				/>
				<OrbitControls makeDefault zoomSpeed={0.5} />
			</Canvas>
		</div>
	);
}
