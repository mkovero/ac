#with 96k
#1024 = 10.7ms, 93.75hz
#512 = 5.33ms, 187.5hz
#256 = 2.67ms, 375hz
#with 48k
#1024 = Temporal resolution = 21.3 ms; FFT bin = ~46.9 Hz.
#512 = 10.7ms, 93.75hz
#256 = 5.33ms, 187.5hz
sudo /usr/local/bin/rt-irq-affinity
sudo echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor > /dev/null
#sudo echo 0 | sudo tee /sys/bus/usb/devices/*/power/autosuspend_delay_ms > /dev/null 2>&1
echo performance gov
RATE="48000"
QUANTUM="128"

pw-metadata -n settings 0 clock.force-quantum $QUANTUM 
pw-metadata -n settings 0 clock.rate $RATE 
env PIPEWIRE_LATENCY="$RATE/$QUANTUM" taskset -c 6,7 chrt -f 70 "$HOME/osm-latest/build/Qt_5_15_16_System-Rel/OpenSoundMeter"
#env PIPEWIRE_LATENCY="$RATE/$QUANTUM" hwloc-bind core:2-3 chrt -f 80 OpenSoundMeter 

sudo echo powersave | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor > /dev/null
echo powersave gov
