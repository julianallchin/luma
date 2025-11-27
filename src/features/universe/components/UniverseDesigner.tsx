import { useEffect } from 'react';
import { SourcePane } from './SourcePane';
import { SimulationPane } from './SimulationPane';
import { AssignmentMatrix } from './AssignmentMatrix';
import { PatchSchedule } from './PatchSchedule';
import { useFixtureStore } from '../stores/use-fixture-store';

export function UniverseDesigner() {
    const initialize = useFixtureStore(state => state.initialize);

    useEffect(() => {
        initialize();
    }, [initialize]);

    return (
        <div className="flex h-full w-full bg-background text-foreground overflow-hidden">
            {/* Left Pane: Source (Search/List/Config) */}
            <div className="w-80 border-r border-border flex-shrink-0 flex flex-col">
                <SourcePane />
            </div>

            {/* Center + Right */}
            <div className="flex-1 flex h-full min-w-0">
                {/* Center Column */}
                <div className="flex-1 flex flex-col h-full min-w-0">
                    {/* Top Right: Simulation */}
                    <div className="h-1/2 border-b border-border relative">
                        <SimulationPane />
                    </div>

                    {/* Bottom Right: Assignment Matrix */}
                    <div className="h-1/2 relative">
                        <AssignmentMatrix />
                    </div>
                </div>

                {/* Patch Schedule Sidebar */}
                <PatchSchedule />
            </div>
        </div>
    );
}
