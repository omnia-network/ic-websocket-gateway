import random
import subprocess
import asyncio
import os

# run 'npm run test:integration'
async def run_integration_test(task_id):    
    cmd = ['npm', 'run', 'test']
    process = await asyncio.create_subprocess_exec(*cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    stdout, stderr = await process.communicate()
    if process.returncode == 0:
        print(f"Task {task_id}: Integration test succeeded.")
    else:
        print(f"Task {task_id}: Integration test failed. Error output:\n{stderr.decode()}")

# Number of tasks to spawn
N = 10

async def run_tasks_with_random_interval():
    # move to ic_websocket_canisters folder
    os.chdir("tests/integration")

    tasks = []
    for i in range(N):
        delay = random.uniform(0, 1)
        await asyncio.sleep(delay)
        task = asyncio.create_task(run_integration_test(i))
        tasks.append(task)

    await asyncio.gather(*tasks)

if __name__ == "__main__":
    asyncio.run(run_tasks_with_random_interval())
