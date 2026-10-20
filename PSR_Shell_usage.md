# Commands & Shell Usage

## First boot
When booting the shell for the first time, the shell will need basic authentication.
Run `login` and then enter `#0000` when `passwd?` is shown.
Don't run this if `> ` appears after pressing enter, the shell may be already logged in from a human.

## Commands
- `logout`: Exit the shell prompt onto the login need state
- `help`: List out the commands that this shell contains
- `?`: Alias for `help`
- `info`: Print basic info about the hardware and firmware
- `ver`: Print out the version information for firmware compoments
- `stack`: Show the stack info for each task in the RTOS Scheduler
- `perf-on`: Enable profiler logging
- `perf-off`: Disable profiler logging
- `perf-disp`: Display the profiler statistics in a grid format
- `d`: Dump the memory from the last specified address, 0x0 if nothing was specified before.
- `dp`: Dump from the final address of the previous `d` or `dp` command
- `d xxxxx`: Dump from the address specified in the `xxxxx` as hexedecimal without the `0x prefix`, setting the address for the `d` command
- `d/s xxxxx`: Set the output dump size of the `d` and `dp` commands in hex, without `0x` prefix. Sizes are rounded to a multiple of 16 bytes.
- `m ADDRESS DATA`: Write `DATA` into the `ADDRESS` of the CPU map
- `m/b ADDRESS DATA`: Write a byte (8 bits) into the CPU memory at the address specified
- `m/w ADDRESS DATA`: Write a word (16 bits) into the CPU memory at the address specified
- `m/l ADDRESS DATA`: Write a long (32 bits) into the CPU memory at the address specified
- `regist`: List info about the currently loaded registration memory block
- `voiceset`: List the settings of the voices, `MAIN`, `DUAL` and `SPLIT`
- `ots`: List the parameters that are used when `OTS` on the pannel is selected (000)
- `func`: List the current function settings
- `mdb`: Enter the internal debugger
- `backup`: List the function parameters that are saved when the keyboard enters the power down routine
- `chkprm`: List the function parameters that are loaded on factory reset
- `freearea`: List the amount of free bytes on the flash memory
- `jobconid`: List the RTOS Jobcon categories and status of each
- `dispinfo`: Display the task responsible for the front panel
- `swmask`: List any panel switches that are currently disabled as a bitmask
- `ioport`: Show the status of the GPIO/I2S pins as a bitmask
- `getsw`: Get the history of the pressed panel buttons
- `pushsw xx`: Add a switch press onto the queue, xx is in hex no 0x prefix
- `sendsw`: Dispatch the switches on the switch queue, presses the panel buttons
- `perf-key`: List profiler results of the keys handler task
- `perf-sw`: List profiler results of the panel buttons handler task
- `perf-pedal`: List profiler results of the pedal handler task
- `perf-wheel`: List profiler results of the pitch-bend handler task
- `perf-vol`: List profiler results of the master volume handler task
### The below require hardware debug switches to be set
- `sound-cnt-start`: Start benchmarking the read and write speed of the program/wave rom
- `sound-cnt-disp`: Display the results of the above benchmark
- `flash-write`: Benchmark the read/write speed of the flash memory and display the results
- `voice-time`: Benchmark the voice loading and display the results
- `regist-time`: Benchmark the read/write speed of the registration memory and display the results

## Notes
Certain Memory Regions will crash the CPU, requiring a power cycle. Use notes and ghirdra to track safe addresses.

The power button is an RS latch, it will start the CPU on power on and send a signal to the CPU on power off so the CPU can perform backup tasks, so in a crash, the power button cannot be used.
