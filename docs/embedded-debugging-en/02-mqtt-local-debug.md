# MQTT says the pump is idle, but it just reported running. Is the message retained?

An MQTT dashboard can briefly show yesterday's state the moment it reconnects. If a pump just published `running` but a new subscriber immediately sees `idle`, check whether the broker still holds an older retained message. You can reproduce that behavior on one computer without connecting a pump, a microcontroller, or a cloud service.

I maintain [QCCOM](https://www.zenghi.com/en/), an Apache-2.0 debug tool for Windows and Linux. I use its MQTT log below to watch incoming topics. The Eclipse Mosquitto command-line clients give us an independent way to inspect the broker's retain flag.

## 1. Start an isolated broker

Install the broker and command-line clients from [Eclipse Mosquitto](https://mosquitto.org/download/). Save the following as `bench-mqtt.conf`:

```conf
listener 18883 127.0.0.1
allow_anonymous true
persistence false
```

Then run the broker in one terminal:

```sh
mosquitto -c bench-mqtt.conf -v
```

Keep the broker bound to `127.0.0.1`. Anonymous, unencrypted access here is only for this loopback exercise; do not expose this listener to the network. Port `18883` keeps the test separate from a broker already listening on the usual `1883`.

## 2. Seed an old status and catch a real update

In a second terminal, publish an artificial idle status with retain enabled:

```sh
mosquitto_pub -h 127.0.0.1 -p 18883 -V mqttv311 -q 1 -r -t bench/pump-a/status -m '{"seq":1,"state":"idle"}'
```

The examples use POSIX shell quoting. If your Windows shell passes JSON quoting differently, put each JSON payload in a UTF-8 file and use `-f filename` instead of `-m`.

In QCCOM v1.9.6, open **Toolbox → Communication → MQTT**. Set host `127.0.0.1`, port `18883`, and client ID `qccomobserver`. Leave TLS off and the username/password empty; keep **Clean session** enabled for this exercise. Connect, then add `bench/+/status` as a subscription. The `+` matches one topic level, such as `pump-a`. The publish topic itself has no wildcard.

You should see `seq:1` on the first subscription even though the publisher has already exited. Now publish a newer reading without `-r`:

```sh
mosquitto_pub -h 127.0.0.1 -p 18883 -V mqttv311 -q 1 -t bench/pump-a/status -m '{"seq":2,"state":"running"}'
```

The live subscriber should receive `seq:2`. Remove and re-add the filter in QCCOM, or use the fresh command-line subscriber in the next step. You can still get `seq:1`: publishing a live reading without retain does not replace a retained value.

## 3. Verify it independently, then clear the lab value

Start a second subscriber that prints the retain flag, topic, and payload. This instance exits after one message, with a two-second timeout:

```sh
mosquitto_sub -h 127.0.0.1 -p 18883 -V mqttv311 -t 'bench/+/status' -C 1 -W 2 -F '%r %t %p'
```

The Mosquitto 2.0.21 terminal test printed this **after** the non-retained `running` update:

```text
1 bench/pump-a/status {"seq":1,"state":"idle"}
```

The leading `1` confirms this was a retained delivery. To print only a fresh reading, add `-R` to the subscriber command, start that subscriber first, and publish `seq:2` again while it waits. The first column should then be `0`.

To clean up **only this lab topic**, send a retained message with a zero-length payload:

```sh
mosquitto_pub -h 127.0.0.1 -p 18883 -V mqttv311 -q 1 -r -n -t bench/pump-a/status
```

A subscriber that was already connected may see an empty message during cleanup. Run the fresh subscriber command again: it should time out without a saved payload. Stop the broker when finished. In a real deployment, first check what consumes a topic before clearing it; systems may use retained state deliberately.

| Symptom | What to check first |
|---|---|
| One old value appears immediately on a new subscription | Inspect the retain flag and compare the payload's timestamp or sequence number. |
| The subscriber receives nothing even when you publish live | Compare broker address, port, and exact topic spelling. MQTT topic names are case-sensitive. |
| Connections drop when a second client starts | Give each simultaneously connected client a distinct client ID. |

**Verification:** The retained, live, and clear sequence was tested against a local Mosquitto 2.0.21 broker using its command-line clients. The QCCOM controls above are described in the [v1.9.6 user guide](https://github.com/zenghi/Qccom/blob/v1.9.6/docs/USER_GUIDE.md#tcp-udp-mqtt-and-websocket); the exact pump example has not been recorded in the QCCOM GUI. This previously recorded [QCCOM/Mosquitto live publish–subscribe demo](https://blog.csdn.net/zenghi/article/details/170985978) shows the client workflow with a different topic.

Further reading: [QCCOM source](https://github.com/zenghi/Qccom/tree/main), [Mosquitto's configuration guide](https://mosquitto.org/man/mosquitto-conf-5.html), [subscriber option reference](https://mosquitto.org/man/mosquitto_sub-1.html), and [MQTT 3.1.1 §3.3.1, including retain semantics](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html).
