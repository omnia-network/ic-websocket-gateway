import subprocess
import asyncio
import os

# run 'npm run test:integration'
async def run_integration_test(task_id):    
    cmd = ['npm', 'run', 'test:integration']
    process = await asyncio.create_subprocess_exec(*cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    stdout, stderr = await process.communicate()
    if process.returncode == 0:
        print(f"Task {task_id}: Integration test succeeded.")
    else:
        print(f"Task {task_id}: Integration test failed. Error output:\n{stderr.decode()}")

# Number of tasks to spawn
N = 10

async def main():
    # move to ic_websocket_canisters folder
    os.chdir("ic_websocket_canisters")

    tasks = [run_integration_test(i) for i in range(N)]
    await asyncio.gather(*tasks)

if __name__ == "__main__":
    asyncio.run(main())
