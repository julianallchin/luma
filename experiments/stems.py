#!/usr/bin/env python3
import sys
import os
import subprocess
import shutil
from pathlib import Path

def main():
    # Read file path from stdin
    file_path = sys.stdin.readline().strip()
    
    if not file_path:
        sys.stderr.write("Error: No file path provided\n")
        sys.exit(1)
    
    if not os.path.exists(file_path):
        sys.stderr.write(f"Error: File not found: {file_path}\n")
        sys.exit(1)
    
    # Get the base name without extension for the output directory
    file_stem = Path(file_path).stem
    
    # Create stems directory
    stems_base = Path("stems")
    stems_base.mkdir(parents=True, exist_ok=True)
    
    # Run demucs with htdemucs_ft model
    # Demucs creates: stems/htdemucs_ft/file_stem/
    try:
        result = subprocess.run(
            ["demucs", "-n", "htdemucs_ft", "--out", str(stems_base), file_path],
            capture_output=True,
            text=True,
            check=True
        )
        
        # Move files from stems/htdemucs_ft/file_stem/ to stems/file_stem/
        source_dir = stems_base / "htdemucs_ft" / file_stem
        target_dir = stems_base / file_stem
        
        if source_dir.exists():
            if target_dir.exists():
                shutil.rmtree(target_dir)
            shutil.move(str(source_dir), str(target_dir))
            
            # Clean up empty htdemucs_ft directory if it exists
            htdemucs_dir = stems_base / "htdemucs_ft"
            if htdemucs_dir.exists() and not any(htdemucs_dir.iterdir()):
                htdemucs_dir.rmdir()
        
        # Output "done" to stdout
        print("done")
        sys.exit(0)
    except subprocess.CalledProcessError as e:
        sys.stderr.write(f"Error during separation: {e.stderr}\n")
        sys.exit(1)
    except FileNotFoundError:
        sys.stderr.write("Error: demucs command not found. Please install demucs: pip install demucs\n")
        sys.exit(1)
    except Exception as e:
        sys.stderr.write(f"Error: {str(e)}\n")
        sys.exit(1)

if __name__ == "__main__":
    main()

