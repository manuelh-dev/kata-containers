# Copyright (c) 2026 NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0

apiVersion: v1
kind: Namespace
metadata:
  name: kata-initdata-webhook
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: kata-initdata-webhook
  namespace: kata-initdata-webhook
spec:
  replicas: 1
  selector:
    matchLabels:
      app: kata-initdata-webhook
  template:
    metadata:
      labels:
        app: kata-initdata-webhook
    spec:
      containers:
        - name: webhook
          image: ${WEBHOOK_IMAGE}
          imagePullPolicy: Never
          args:
            - -tls-cert-file=/etc/webhook/certs/cert.pem
            - -tls-key-file=/etc/webhook/certs/key.pem
            - -only-namespaces-regexp=^default$
          ports:
            - name: https
              containerPort: 8080
          readinessProbe:
            tcpSocket:
              port: https
          securityContext:
            allowPrivilegeEscalation: false
            capabilities:
              drop:
                - ALL
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            runAsUser: 1000
            seccompProfile:
              type: RuntimeDefault
          volumeMounts:
            - name: certs
              mountPath: /etc/webhook/certs
              readOnly: true
      volumes:
        - name: certs
          secret:
            secretName: pod-annotate-webhook-certs
---
apiVersion: v1
kind: Service
metadata:
  name: pod-annotate-webhook
  namespace: kata-initdata-webhook
spec:
  selector:
    app: kata-initdata-webhook
  ports:
    - port: 443
      targetPort: https
---
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: kata-initdata-webhook
webhooks:
  - name: initdata.kata-containers.io
    admissionReviewVersions:
      - v1
    sideEffects: None
    failurePolicy: Fail
    reinvocationPolicy: IfNeeded
    timeoutSeconds: 10
    namespaceSelector:
      matchLabels:
        kata-containers.io/initdata-webhook: enabled
    matchConditions:
      - name: has-kata-init-data
        expression: >-
          has(object.metadata.annotations) &&
          'io.katacontainers.config.hypervisor.cc_init_data' in object.metadata.annotations
    clientConfig:
      service:
        name: pod-annotate-webhook
        namespace: kata-initdata-webhook
        path: /mutate
      caBundle: ${CA_BUNDLE}
    rules:
      - operations:
          - CREATE
        apiGroups:
          - ""
        apiVersions:
          - v1
        resources:
          - pods
